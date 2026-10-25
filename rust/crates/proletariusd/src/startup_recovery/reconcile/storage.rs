//! Exclusive, immutable recovery records and exact original-directory placement.
#[cfg(test)]
use super::crash_tests::{PublicationPoint, observe_publication};
use super::ledger::{DOMAIN, Envelope, Error, Event};
use crate::daemon_lifetime::RootProof;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, fstatat, mkdirat};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::File;
#[cfg(not(test))]
use std::io::Write;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

pub(crate) const NAME: &str = "module-recovery-v1";
pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_NODES: usize = 768;
const MAX_RECORD: usize = 32 * 1024;
const MAX_MANIFEST: usize = 512 * 1024;
const MAX_ATTEMPTS: usize = 8;

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn invalid() -> io::Error {
    io::Error::other("reconciliation namespace or immutable record changed")
}
fn safe_dir(file: &File) -> io::Result<()> {
    let m = file.metadata()?;
    if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn directory(parent: &File, name: &str) -> io::Result<File> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(invalid());
    }
    let named = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    let f = File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    safe_dir(&f)?;
    let m = f.metadata()?;
    if named.st_dev as u64 != m.dev() || named.st_ino as u64 != m.ino() {
        return Err(invalid());
    }
    Ok(f)
}
pub(super) fn optional_dir(parent: &File, name: &str) -> io::Result<Option<File>> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(e) => Err(e.into()),
        Ok(_) => directory(parent, name).map(Some),
    }
}
fn create_dir(parent: &File, name: &str) -> io::Result<File> {
    mkdirat(parent, name, Mode::S_IRWXU)?;
    parent.sync_all()?;
    directory(parent, name)
}
fn names(f: &File, limit: usize) -> io::Result<Vec<String>> {
    let mut n = proletarius_platform_fs::directory_names(f, limit)?
        .into_iter()
        .map(|n| n.into_string().map_err(|_| invalid()))
        .collect::<io::Result<Vec<_>>>()?;
    n.sort();
    Ok(n)
}
fn read(parent: &File, name: &str, limit: usize) -> io::Result<Vec<u8>> {
    let named = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    let f = File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    let m = f.metadata()?;
    if !m.is_file()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.len() == 0
        || m.len() > limit as u64
        || named.st_dev as u64 != m.dev()
        || named.st_ino as u64 != m.ino()
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    (&f).take(m.len() + 1).read_to_end(&mut bytes)?;
    let after = f.metadata()?;
    let again = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    if bytes.len() as u64 != m.len()
        || stamp(&m) != stamp(&after)
        || again.st_ino as u64 != m.ino()
        || again.st_dev as u64 != m.dev()
    {
        return Err(invalid());
    }
    Ok(bytes)
}
fn stamp(m: &std::fs::Metadata) -> (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn write_new(parent: &File, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut f = File::from(openat(
        parent,
        name,
        OFlag::O_RDWR
            | OFlag::O_CREAT
            | OFlag::O_EXCL
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )?);
    #[cfg(test)]
    super::finite_io_tests::write_all(parent, name, &mut f, bytes)?;
    #[cfg(not(test))]
    f.write_all(bytes)?;
    #[cfg(test)]
    {
        super::finite_io_tests::sync_all(super::finite_io_tests::SyncPoint::File, &f)?;
        super::finite_io_tests::sync_all(super::finite_io_tests::SyncPoint::RecordParent, parent)?;
    }
    #[cfg(not(test))]
    {
        f.sync_all()?;
        parent.sync_all()?;
    }
    if read(parent, name, bytes.len())? != bytes {
        return Err(invalid());
    }
    Ok(())
}
fn walk(f: &File, depth: usize, nodes: &mut usize, bytes: &mut usize) -> io::Result<()> {
    if depth > 5 {
        return Err(invalid());
    }
    safe_dir(f)?;
    for n in names(f, MAX_NODES)? {
        *nodes += 1;
        if *nodes > MAX_NODES {
            return Err(invalid());
        }
        let st = fstatat(f, n.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
        if st.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR {
            if depth == 0 && n == super::backup_lifecycle::NAME {
                // The lifecycle owns a separate typed parser, including its
                // zero-byte private flock and fixed stage/archive filenames.
                continue;
            }
            let child = directory(f, &n)?;
            if n == "recovered-original" {
                for name in names(&child, 2)? {
                    if !matches!(name.as_str(), "journal" | "voice-control") {
                        return Err(invalid());
                    }
                    directory(&child, &name)?;
                }
            } else {
                walk(&child, depth + 1, nodes, bytes)?;
            }
        } else {
            let raw = read(f, &n, MAX_MANIFEST)?;
            *bytes = bytes.checked_add(raw.len()).ok_or_else(invalid)?;
            if *bytes > MAX_BYTES {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
pub(super) fn validate_budget(root: &File) -> io::Result<()> {
    if let Some(ns) = optional_dir(root, NAME)? {
        let (mut n, mut b) = (0, 0);
        walk(&ns, 0, &mut n, &mut b)?;
        let (lifecycle_nodes, lifecycle_bytes) = super::backup_lifecycle::budget_usage(root)?;
        if n.checked_add(lifecycle_nodes).is_none_or(|v| v > MAX_NODES)
            || b.checked_add(lifecycle_bytes)
                .is_none_or(|v| v > 8 * 1024 * 1024)
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub(super) struct Store {
    root: PathBuf,
    proof: RootProof,
    namespace: File,
    attempt: File,
    archive: File,
    id: String,
    root_id: (u64, u64),
    namespace_id: (u64, u64),
    attempt_id: (u64, u64),
    archive_id: (u64, u64),
    files: Vec<(String, Vec<u8>)>,
    records: Vec<Envelope>,
    failed: bool,
}
impl Store {
    pub(super) fn create(
        root: &Path,
        proof: &RootProof,
        id: &str,
        manifest: &Value,
    ) -> Result<Self, Error> {
        if !valid_id(id) {
            return Err(Error);
        }
        proof
            .with_root(root, |held| -> io::Result<Self> {
                validate_budget(held)?;
                let namespace = match optional_dir(held, NAME)? {
                    Some(f) => f,
                    None => create_dir(held, NAME)?,
                };
                let attempts = names(&namespace, MAX_ATTEMPTS + 2)?
                    .into_iter()
                    .filter(|n| n != "origins" && n != super::backup_lifecycle::NAME)
                    .collect::<Vec<_>>();
                if attempts.len() >= MAX_ATTEMPTS || attempts.iter().any(|n| !valid_id(n)) {
                    return Err(invalid());
                }
                let attempt = create_dir(&namespace, id)?;
                let archive = create_dir(&attempt, "recovered-original")?;
                let original = serde_json::to_vec(manifest).map_err(|_| invalid())?;
                if original.len() > MAX_MANIFEST {
                    return Err(invalid());
                }
                write_new(&attempt, "original-manifest.json", &original)?;
                held.sync_all()?;
                let identity = |f: &File| -> io::Result<(u64, u64)> {
                    let m = f.metadata()?;
                    Ok((m.dev(), m.ino()))
                };
                Ok(Self {
                    root: root.to_owned(),
                    proof: proof.clone(),
                    root_id: identity(held)?,
                    namespace_id: identity(&namespace)?,
                    attempt_id: identity(&attempt)?,
                    archive_id: identity(&archive)?,
                    namespace,
                    attempt,
                    archive,
                    id: id.to_owned(),
                    files: vec![("original-manifest.json".into(), original)],
                    records: Vec::new(),
                    failed: false,
                })
            })
            .map_err(|_| Error)?
            .map_err(|_| Error)
    }
    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.failed {
            return Err(Error);
        }
        self.proof
            .with_root(&self.root, |held| -> io::Result<()> {
                let ns = directory(held, NAME)?;
                let at = directory(&ns, &self.id)?;
                let ar = directory(&at, "recovered-original")?;
                for (f, id) in [
                    (held, self.root_id),
                    (&ns, self.namespace_id),
                    (&at, self.attempt_id),
                    (&ar, self.archive_id),
                ] {
                    let m = f.metadata()?;
                    if (m.dev(), m.ino()) != id {
                        return Err(invalid());
                    }
                }
                let mut expected = self
                    .files
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>();
                expected.push("recovered-original".into());
                expected.sort();
                if names(&self.attempt, 256)? != expected {
                    return Err(invalid());
                }
                for (n, b) in &self.files {
                    if read(&self.attempt, n, MAX_MANIFEST)? != *b {
                        return Err(invalid());
                    }
                }
                validate_budget(held)
            })
            .map_err(|_| Error)?
            .map_err(|_| Error)
    }
    pub(super) fn write_value(&mut self, name: &str, value: &Value) -> Result<String, Error> {
        self.validate()?;
        #[cfg(test)]
        let _finite_io_context = super::finite_io_tests::ContextGuard::reconciliation(
            &self.proof,
            &self.root,
            &self.attempt,
            &self.id,
            self.records.len() as u64 + 1,
            if name == "published-manifest.json" {
                super::finite_io_tests::Record::PublishedManifest
            } else {
                super::finite_io_tests::Record::Other
            },
            super::finite_io_tests::Mode::Mutation,
        )
        .map_err(|_| Error)?;
        let raw = serde_json::to_vec(value).map_err(|_| Error)?;
        if raw.len() > MAX_MANIFEST {
            return Err(Error);
        }
        let result = write_new(&self.attempt, name, &raw).map_err(|_| Error);
        if result.is_err() {
            self.failed = true;
            return Err(Error);
        }
        self.files.push((name.into(), raw.clone()));
        self.proof
            .with_root(&self.root, |r| r.sync_all())
            .map_err(|_| Error)?
            .map_err(|_| Error)?;
        self.validate()?;
        Ok(digest(&raw))
    }
    pub(super) fn append(&mut self, event: Event) -> Result<String, Error> {
        self.validate()?;
        #[cfg(test)]
        let _finite_io_context = super::finite_io_tests::ContextGuard::reconciliation(
            &self.proof,
            &self.root,
            &self.attempt,
            &self.id,
            self.records.len() as u64 + 1,
            match &event {
                Event::Requested { .. } => super::finite_io_tests::Record::Requested,
                Event::CompletionReceipt { .. } => {
                    super::finite_io_tests::Record::CompletionReceipt
                }
                _ => super::finite_io_tests::Record::Other,
            },
            super::finite_io_tests::Mode::Mutation,
        )
        .map_err(|_| Error)?;
        if self.records.len() >= 128 {
            return Err(Error);
        }
        let row = Envelope {
            schema: 1,
            domain: DOMAIN.into(),
            attempt_id: self.id.clone(),
            sequence: self.records.len() as u64 + 1,
            previous_sha256: self
                .records
                .last()
                .map(|last| digest(&serde_json::to_vec(last).expect("validated encodable record"))),
            event,
        };
        let raw = serde_json::to_vec(&row).map_err(|_| Error)?;
        if raw.len() > MAX_RECORD {
            return Err(Error);
        }
        // Reserve enough space for final receipts before every new physical Prepare.
        let (mut count, mut used) = (0, 0);
        walk(&self.namespace, 0, &mut count, &mut used).map_err(|_| Error)?;
        if used + raw.len() + 512 * 1024 > MAX_BYTES || count + 16 > MAX_NODES {
            return Err(Error);
        }
        let name = format!("{:06}.json", row.sequence);
        if write_new(&self.attempt, &name, &raw).is_err() {
            self.failed = true;
            return Err(Error);
        }
        self.files.push((name, raw.clone()));
        self.records.push(row);
        self.proof
            .with_root(&self.root, |r| {
                #[cfg(test)]
                {
                    super::finite_io_tests::sync_all(
                        super::finite_io_tests::SyncPoint::AppendRoot,
                        r,
                    )
                }
                #[cfg(not(test))]
                {
                    r.sync_all()
                }
            })
            .map_err(|_| Error)?
            .map_err(|_| Error)?;
        self.validate()?;
        Ok(digest(&raw))
    }
    pub(super) fn archive_parent(&self) -> Result<File, Error> {
        self.validate()?;
        self.archive.try_clone().map_err(|_| Error)
    }
    pub(super) fn retire_slot(&mut self, slot: &str, archive: &str) -> Result<(), Error> {
        if !matches!(
            (slot, archive),
            ("call-mode-restore.active", "journal") | ("voice-control", "voice-control")
        ) {
            return Err(Error);
        }
        self.validate()?;
        self.proof
            .with_root(&self.root, |held| -> io::Result<()> {
                #[cfg(test)]
                let _finite_io_context = super::finite_io_tests::ContextGuard::reconciliation_held(
                    held,
                    &self.attempt,
                    &self.id,
                    self.records.len() as u64,
                    if archive == "journal" {
                        super::finite_io_tests::Record::JournalRetirement
                    } else {
                        super::finite_io_tests::Record::Other
                    },
                    super::finite_io_tests::Mode::Mutation,
                )?;
                #[cfg(test)]
                observe_publication(PublicationPoint::BeforeRename, archive)?;
                proletarius_platform_fs::rename_directory_noreplace(
                    held,
                    slot,
                    &self.archive,
                    archive,
                )?;
                #[cfg(test)]
                observe_publication(PublicationPoint::AfterRename, archive)?;
                #[cfg(test)]
                observe_publication(PublicationPoint::BeforeArchiveSync, archive)?;
                #[cfg(test)]
                super::finite_io_tests::sync_all(
                    super::finite_io_tests::SyncPoint::RecoveryArchive,
                    &self.archive,
                )?;
                #[cfg(not(test))]
                self.archive.sync_all()?;
                #[cfg(test)]
                observe_publication(PublicationPoint::AfterArchiveSync, archive)?;
                #[cfg(test)]
                observe_publication(PublicationPoint::BeforeAttemptSync, archive)?;
                self.attempt.sync_all()?;
                #[cfg(test)]
                observe_publication(PublicationPoint::AfterAttemptSync, archive)?;
                #[cfg(test)]
                observe_publication(PublicationPoint::BeforeNamespaceSync, archive)?;
                self.namespace.sync_all()?;
                #[cfg(test)]
                observe_publication(PublicationPoint::AfterNamespaceSync, archive)?;
                #[cfg(test)]
                observe_publication(PublicationPoint::BeforeRootSync, archive)?;
                held.sync_all()?;
                #[cfg(test)]
                observe_publication(PublicationPoint::AfterRootSync, archive)?;
                Ok(())
            })
            .map_err(|_| Error)?
            .map_err(|_| Error)
    }
    pub(super) fn id(&self) -> &str {
        &self.id
    }
    pub(super) fn receipt_sha(&self) -> Option<String> {
        self.records
            .last()
            .map(|r| digest(&serde_json::to_vec(r).expect("encodable")))
    }
}

pub(super) struct Attempt {
    pub(super) id: String,
    pub(super) directory: File,
    pub(super) archive: File,
    pub(super) records: Vec<Envelope>,
    pub(super) original: Value,
    pub(super) published: Option<Value>,
}
pub(super) fn attempts(root: &File) -> Result<Option<Vec<Attempt>>, Error> {
    read_attempts(root, true)
}
// Bounded original-publication reader. It validates the original recovery
// namespace without recursively parsing the caller's lifecycle transaction.
// This cannot create a permit, scope, receipt or live recovery authority.
pub(super) fn lifecycle_origin_attempts(root: &File) -> Result<Option<Vec<Attempt>>, Error> {
    read_attempts(root, false)
}
fn read_attempts(root: &File, include_lifecycle: bool) -> Result<Option<Vec<Attempt>>, Error> {
    let Some(ns) = optional_dir(root, NAME).map_err(|_| Error)? else {
        return Ok(None);
    };
    if include_lifecycle {
        validate_budget(root).map_err(|_| Error)?;
    } else {
        let (mut n, mut b) = (0, 0);
        walk(&ns, 0, &mut n, &mut b).map_err(|_| Error)?;
    }
    let mut out = Vec::new();
    for id in names(&ns, MAX_ATTEMPTS + 2).map_err(|_| Error)? {
        if id == "origins" || id == super::backup_lifecycle::NAME {
            continue;
        }
        if !valid_id(&id) {
            return Err(Error);
        }
        let at = directory(&ns, &id).map_err(|_| Error)?;
        let ar = directory(&at, "recovered-original").map_err(|_| Error)?;
        let original = read_value(&at, "original-manifest.json", MAX_MANIFEST)?;
        let mut records = Vec::<Envelope>::new();
        let mut prev = None;
        let all = names(&at, 256).map_err(|_| Error)?;
        for name in &all {
            if matches!(
                name.as_str(),
                "original-manifest.json"
                    | "published-manifest.json"
                    | "protective-backup.json"
                    | "recovered-original"
            ) {
                continue;
            }
            let expect = format!("{:06}.json", records.len() + 1);
            if *name != expect {
                return Err(Error);
            }
            let raw = read(&at, name, MAX_RECORD).map_err(|_| Error)?;
            let row: Envelope = serde_json::from_slice(&raw).map_err(|_| Error)?;
            if serde_json::to_vec(&row).map_err(|_| Error)? != raw {
                return Err(Error);
            }
            if row.schema != 1
                || row.domain != DOMAIN
                || row.attempt_id != id
                || row.sequence != records.len() as u64 + 1
                || row.previous_sha256 != prev
            {
                return Err(Error);
            }
            prev = Some(digest(&raw));
            records.push(row);
        }
        let published = if all.iter().any(|n| n == "published-manifest.json") {
            Some(read_value(&at, "published-manifest.json", MAX_MANIFEST)?)
        } else {
            None
        };
        out.push(Attempt {
            id,
            directory: at,
            archive: ar,
            records,
            original,
            published,
        });
    }
    Ok(Some(out))
}

pub(super) fn optional_archive_voice(parent: &File) -> Result<Option<File>, Error> {
    optional_dir(parent, "voice-control").map_err(|_| Error)
}
pub(super) fn write_origin(root: &File, value: &Value) -> io::Result<()> {
    let ns = directory(root, NAME)?;
    let origins = match optional_dir(&ns, "origins")? {
        Some(f) => f,
        None => create_dir(&ns, "origins")?,
    };
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    let name = format!("{}.json", digest(&bytes));
    match read(&origins, &name, MAX_RECORD) {
        Ok(old) if old == bytes => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => write_new(&origins, &name, &bytes),
        _ => Err(invalid()),
    }?;
    ns.sync_all()?;
    root.sync_all()?;
    validate_budget(root)
}

impl Store {
    pub(super) fn resume(root: &Path, proof: &RootProof, old: Attempt) -> Result<Self, Error> {
        proof
            .with_root(root, |held| -> io::Result<Self> {
                let namespace = directory(held, NAME)?;
                let attempt = directory(&namespace, &old.id)?;
                let archive = directory(&attempt, "recovered-original")?;
                for (f, old_fd) in [(&attempt, &old.directory), (&archive, &old.archive)] {
                    let a = f.metadata()?;
                    let b = old_fd.metadata()?;
                    if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                        return Err(invalid());
                    }
                }
                if read_value(&attempt, "original-manifest.json", MAX_MANIFEST)
                    .map_err(|_| invalid())?
                    != old.original
                {
                    return Err(invalid());
                }
                for row in &old.records {
                    if read(&attempt, &format!("{:06}.json", row.sequence), MAX_RECORD)?
                        != serde_json::to_vec(row).map_err(|_| invalid())?
                    {
                        return Err(invalid());
                    }
                }
                let files = names(&attempt, 256)?
                    .into_iter()
                    .filter(|n| n != "recovered-original")
                    .map(|name| read(&attempt, &name, MAX_MANIFEST).map(|raw| (name, raw)))
                    .collect::<io::Result<Vec<_>>>()?;
                let identity = |f: &File| -> io::Result<(u64, u64)> {
                    let m = f.metadata()?;
                    Ok((m.dev(), m.ino()))
                };
                Ok(Self {
                    root: root.into(),
                    proof: proof.clone(),
                    root_id: identity(held)?,
                    namespace_id: identity(&namespace)?,
                    attempt_id: identity(&attempt)?,
                    archive_id: identity(&archive)?,
                    namespace,
                    attempt,
                    archive,
                    id: old.id,
                    files,
                    records: old.records,
                    failed: false,
                })
            })
            .map_err(|_| Error)?
            .map_err(|_| Error)
    }
    pub(super) fn records(&self) -> &[Envelope] {
        &self.records
    }
    pub(super) fn sync_publication_parents(&self) -> Result<(), Error> {
        self.archive.sync_all().map_err(|_| Error)?;
        self.attempt.sync_all().map_err(|_| Error)?;
        self.namespace.sync_all().map_err(|_| Error)?;
        self.proof
            .with_root(&self.root, |held| held.sync_all())
            .map_err(|_| Error)?
            .map_err(|_| Error)
    }
}
impl Store {
    pub(super) fn write_or_verify_value(
        &mut self,
        name: &str,
        value: &Value,
    ) -> Result<String, Error> {
        self.validate()?;
        let bytes = serde_json::to_vec(value).map_err(|_| Error)?;
        if let Some((_, old)) = self.files.iter().find(|(n, _)| n == name) {
            if *old != bytes {
                return Err(Error);
            }
            return Ok(digest(old));
        }
        self.write_value(name, value)
    }
}
pub(super) fn origins(root: &File) -> Result<Vec<Value>, Error> {
    let Some(ns) = optional_dir(root, NAME).map_err(|_| Error)? else {
        return Ok(Vec::new());
    };
    let Some(dir) = optional_dir(&ns, "origins").map_err(|_| Error)? else {
        return Ok(Vec::new());
    };
    let before = stamp(&dir.metadata().map_err(|_| Error)?);
    let mut out = Vec::new();
    let all = names(&dir, 256).map_err(|_| Error)?;
    for name in &all {
        let raw = read(&dir, name, MAX_RECORD).map_err(|_| Error)?;
        if *name != format!("{}.json", digest(&raw)) {
            return Err(Error);
        }
        out.push(serde_json::from_slice(&raw).map_err(|_| Error)?);
    }
    if names(&dir, 256).map_err(|_| Error)? != all
        || stamp(&dir.metadata().map_err(|_| Error)?) != before
    {
        return Err(Error);
    }
    Ok(out)
}
impl Store {
    pub(super) fn with_root<T>(
        &self,
        read: impl FnOnce(&File) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.proof.with_root(&self.root, read).map_err(|_| Error)?
    }
}

fn read_value(parent: &File, name: &str, limit: usize) -> Result<Value, Error> {
    let raw = read(parent, name, limit).map_err(|_| Error)?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| Error)?;
    if serde_json::to_vec(&value).map_err(|_| Error)? != raw {
        return Err(Error);
    }
    Ok(value)
}

// Reserve an entire immutable generation while the caller holds the one real
// lifecycle inventory flock. Exhaustion is checked before the Intent exists.
pub(super) fn reserve_lifecycle_budget(
    root: &File,
    added_nodes: usize,
    added_bytes: usize,
) -> io::Result<()> {
    let (mut nodes, mut bytes) = (0, 0);
    if let Some(ns) = optional_dir(root, NAME)? {
        walk(&ns, 0, &mut nodes, &mut bytes)?;
    }
    let (n, b) = super::backup_lifecycle::budget_usage(root)?;
    if nodes
        .checked_add(n)
        .and_then(|v| v.checked_add(added_nodes))
        .is_none_or(|v| v > MAX_NODES)
        || bytes
            .checked_add(b)
            .and_then(|v| v.checked_add(added_bytes))
            .is_none_or(|v| v > 8 * 1024 * 1024)
    {
        return Err(io::Error::other(
            "backup lifecycle shared durable capacity exhausted",
        ));
    }
    Ok(())
}

// A transient, bounded I/O snapshot, never a cached permission or a root
// lifetime owner. Keep raw key-bearing evidence zeroized and descriptors only
// for the current recursive path plus one regular file. The existing strict
// typed classifier must succeed before this snapshot is used for resync.
type MetadataStamp = (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64);
// Deliberate over-approximations of independent existing parser budgets, plus
// their containing directories. They cannot relax those prior typed checks.
const METADATA_NODES: usize = MAX_NODES + 512 + 512 + 16;
const METADATA_BYTES: usize = MAX_BYTES + 4 * 1024 * 1024 + 8 * 1024 * 1024;
#[derive(PartialEq, Eq)]
struct MetadataNode {
    name: OsString,
    stamp: MetadataStamp,
    raw: zeroize::Zeroizing<Vec<u8>>,
    children: Vec<MetadataNode>,
    sync: bool,
}
#[derive(PartialEq, Eq)]
pub(super) struct MetadataSnapshot {
    root: MetadataStamp,
    entries: Vec<(String, Option<MetadataNode>)>,
}
struct MetadataBudget {
    nodes: usize,
    bytes: usize,
}
fn metadata_named_stamp(parent: &File, name: &OsStr) -> io::Result<MetadataStamp> {
    let m = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    if m.st_size < 0 {
        return Err(invalid());
    }
    Ok((
        m.st_dev as u64,
        m.st_ino as u64,
        m.st_mode as u32,
        m.st_uid,
        m.st_gid,
        m.st_nlink as u64,
        m.st_size as u64,
        m.st_mtime as i64,
        m.st_mtime_nsec as i64,
        m.st_ctime as i64,
        m.st_ctime_nsec as i64,
    ))
}
fn metadata_open(parent: &File, name: &OsStr, expected: MetadataStamp) -> io::Result<File> {
    if name.is_empty()
        || name.as_bytes().contains(&b'/')
        || name == "."
        || name == ".."
        || metadata_named_stamp(parent, name)? != expected
    {
        return Err(invalid());
    }
    let directory = expected.2 & nix::libc::S_IFMT as u32 == nix::libc::S_IFDIR as u32;
    let regular = expected.2 & nix::libc::S_IFMT as u32 == nix::libc::S_IFREG as u32;
    if expected.3 != nix::unistd::geteuid().as_raw()
        || expected.2 & 0o7022 != 0
        || !(directory || (regular && expected.5 == 1))
    {
        return Err(invalid());
    }
    let file = File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK
            | if directory {
                OFlag::O_DIRECTORY
            } else {
                OFlag::empty()
            },
        Mode::empty(),
    )?);
    if stamp(&file.metadata()?) != expected || metadata_named_stamp(parent, name)? != expected {
        return Err(invalid());
    }
    Ok(file)
}
fn metadata_raw(file: &File, expected: MetadataStamp) -> io::Result<zeroize::Zeroizing<Vec<u8>>> {
    if expected.6 > 1_100_000 || stamp(&file.metadata()?) != expected {
        return Err(invalid());
    }
    let mut raw = zeroize::Zeroizing::new(vec![0; expected.6 as usize]);
    file.read_exact_at(raw.as_mut_slice(), 0)?;
    if stamp(&file.metadata()?) != expected {
        return Err(invalid());
    }
    Ok(raw)
}
fn metadata_names(directory: &File) -> io::Result<Vec<OsString>> {
    let mut all = proletarius_platform_fs::directory_names(directory, METADATA_NODES)?;
    all.sort();
    if all.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid());
    }
    Ok(all)
}
impl MetadataNode {
    fn capture(
        parent: &File,
        name: &OsStr,
        depth: usize,
        sync: bool,
        budget: &mut MetadataBudget,
    ) -> io::Result<Self> {
        // This captures the recovery, lifecycle and original trees together.
        // Their original typed budgets still govern acceptance independently.
        if depth > 10 || budget.nodes >= METADATA_NODES {
            return Err(invalid());
        }
        budget.nodes += 1;
        let expected = metadata_named_stamp(parent, name)?;
        let file = metadata_open(parent, name, expected)?;
        let directory = file.metadata()?.is_dir();
        let mut raw = zeroize::Zeroizing::new(Vec::new());
        let mut children = Vec::new();
        if directory {
            let all = metadata_names(&file)?;
            for child in &all {
                // The lifecycle has its own typed, actual-FD durability fence.
                // Snapshot it in full, but never reacquire its inventory flock
                // or repeat its I/O here while a normal writer holds that lock.
                let child_sync = sync
                    && !(depth == 0
                        && name == OsStr::new(NAME)
                        && child.as_os_str() == OsStr::new(super::backup_lifecycle::NAME));
                children.push(Self::capture(&file, child, depth + 1, child_sync, budget)?);
            }
            if metadata_names(&file)? != all {
                return Err(invalid());
            }
        } else {
            raw = metadata_raw(&file, expected)?;
            budget.bytes = budget.bytes.checked_add(raw.len()).ok_or_else(invalid)?;
            if budget.bytes > METADATA_BYTES {
                return Err(invalid());
            }
        }
        if stamp(&file.metadata()?) != expected || metadata_named_stamp(parent, name)? != expected {
            return Err(invalid());
        }
        Ok(Self {
            name: name.into(),
            stamp: expected,
            raw,
            children,
            sync,
        })
    }
    fn sync(&self, root: &File, parent: &File) -> io::Result<()> {
        let file = metadata_open(parent, &self.name, self.stamp)?;
        #[cfg(test)]
        let _finite_io_context = super::finite_io_tests::ContextGuard::metadata(
            root,
            parent,
            match serde_json::from_slice::<Envelope>(&self.raw) {
                Ok(Envelope {
                    event: Event::CompletionReceipt { .. },
                    ..
                }) => super::finite_io_tests::Record::CompletionReceipt,
                _ => super::finite_io_tests::Record::Other,
            },
        )?;
        if file.metadata()?.is_dir() {
            let expected = self
                .children
                .iter()
                .map(|n| n.name.clone())
                .collect::<Vec<_>>();
            if metadata_names(&file)? != expected {
                return Err(invalid());
            }
            for child in &self.children {
                child.sync(root, &file)?;
            }
            if self.sync {
                #[cfg(test)]
                super::finite_io_tests::sync_all(
                    super::finite_io_tests::SyncPoint::ResyncContainer,
                    &file,
                )?;
                #[cfg(not(test))]
                file.sync_all()?;
            }
            if metadata_names(&file)? != expected {
                return Err(invalid());
            }
        } else {
            if metadata_raw(&file, self.stamp)? != self.raw {
                return Err(invalid());
            }
            if self.sync {
                #[cfg(test)]
                super::finite_io_tests::sync_all(
                    super::finite_io_tests::SyncPoint::ResyncRecord,
                    &file,
                )?;
                #[cfg(not(test))]
                file.sync_all()?;
            }
            if metadata_raw(&file, self.stamp)? != self.raw {
                return Err(invalid());
            }
        }
        if stamp(&file.metadata()?) != self.stamp
            || metadata_named_stamp(parent, &self.name)? != self.stamp
        {
            return Err(invalid());
        }
        Ok(())
    }
}
impl MetadataSnapshot {
    pub(super) fn capture(root: &File) -> io::Result<Self> {
        safe_dir(root)?;
        let before = stamp(&root.metadata()?);
        let mut budget = MetadataBudget { nodes: 0, bytes: 0 };
        let mut entries = Vec::new();
        // Exactly the original eight policy slots and the typed recovery
        // namespace. No unrelated cache/service/runtime subtree is traversed.
        for name in [
            crate::module_recovery::LEGACY_EXECUTION_PENDING,
            ".call-mode-pristine-v1",
            "call-mode-operation.lock",
            ".module-adb-interface.lock",
            "module-backups",
            "voice-runtime",
            crate::call_mode_restore_journal::ACTIVE_NAME,
            "voice-control",
            NAME,
        ] {
            let node = match fstatat(root, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(_) => Some(MetadataNode::capture(
                    root,
                    OsStr::new(name),
                    0,
                    name == NAME,
                    &mut budget,
                )?),
                Err(nix::errno::Errno::ENOENT) => None,
                Err(e) => return Err(e.into()),
            };
            entries.push((name.into(), node));
        }
        if stamp(&root.metadata()?) != before {
            return Err(invalid());
        }
        Ok(Self {
            root: before,
            entries,
        })
    }
    pub(super) fn verify(&self, root: &File) -> io::Result<()> {
        if Self::capture(root)? != *self {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn sync_and_verify(&self, root: &File) -> io::Result<()> {
        if stamp(&root.metadata()?) != self.root {
            return Err(invalid());
        }
        for (_, node) in &self.entries {
            if let Some(node) = node {
                node.sync(root, root)?;
            }
        }
        #[cfg(test)]
        {
            let _finite_io_context = super::finite_io_tests::ContextGuard::metadata(
                root,
                root,
                super::finite_io_tests::Record::Other,
            )?;
            super::finite_io_tests::sync_all(super::finite_io_tests::SyncPoint::ResyncRoot, root)?;
        }
        #[cfg(not(test))]
        root.sync_all()?;
        self.verify(root)
    }
}
