//! Bounded raw evidence, not a parser's assertion that recovery is safe.
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{FileStat, Mode, fstatat};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use zeroize::Zeroize;

const MAX_NODES: usize = 256;
const MAX_DEPTH: usize = 4;
const MAX_FILE_BYTES: u64 = 1_100_000;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const NAMES: [&str; 8] = [
    crate::module_recovery::LEGACY_EXECUTION_PENDING,
    ".call-mode-pristine-v1",
    "call-mode-operation.lock",
    ".module-adb-interface.lock",
    "module-backups",
    "voice-runtime",
    crate::call_mode_restore_journal::ACTIVE_NAME,
    "voice-control",
];

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: i128,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Stamp {
    fn of(file: &File) -> io::Result<Self> {
        Ok(Self::metadata(&file.metadata()?))
    }
    fn metadata(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            links: m.nlink(),
            size: i128::from(m.len()),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }
    fn stat(m: &FileStat) -> Self {
        Self {
            dev: m.st_dev as u64,
            ino: m.st_ino as u64,
            mode: m.st_mode as u32,
            uid: m.st_uid,
            gid: m.st_gid,
            links: m.st_nlink as u64,
            size: i128::from(m.st_size),
            mtime: (m.st_mtime as i64, m.st_mtime_nsec as i64),
            ctime: (m.st_ctime as i64, m.st_ctime_nsec as i64),
        }
    }
    fn directory(self) -> bool {
        self.mode & nix::libc::S_IFMT as u32 == nix::libc::S_IFDIR as u32
    }
    fn regular(self) -> bool {
        self.mode & nix::libc::S_IFMT as u32 == nix::libc::S_IFREG as u32
    }
    fn safe(self) -> bool {
        // Read/execute bits on pinned runtime resources are compatible with
        // their historical manifest. The containing root must itself be 0700;
        // no evidence inode may be writable by another uid or use special bits.
        self.uid == nix::unistd::geteuid().as_raw()
            && self.mode & 0o7022 == 0
            && (self.directory() || (self.regular() && self.links == 1))
    }
}

fn entry(parent: &File, name: &OsStr) -> io::Result<Option<Stamp>> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(value) => Ok(Some(Stamp::stat(&value))),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn names(directory: &File, limit: usize) -> io::Result<BTreeSet<OsString>> {
    let values = proletarius_platform_fs::directory_names(directory, limit)?;
    let count = values.len();
    let set: BTreeSet<_> = values.into_iter().collect();
    if set.len() != count {
        return Err(invalid("duplicate recovery evidence name"));
    }
    Ok(set)
}

struct Node {
    name: OsString,
    file: File,
    stamp: Stamp,
    bytes: zeroize::Zeroizing<Vec<u8>>,
    sha256: [u8; 32],
    children: Vec<Node>,
    names: BTreeSet<OsString>,
}
impl Drop for Node {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

struct Budget {
    limit: usize,
    nodes: usize,
    bytes: usize,
}
impl Node {
    fn capture(
        parent: &File,
        name: OsString,
        depth: usize,
        budget: &mut Budget,
        check: &impl Fn() -> io::Result<()>,
    ) -> io::Result<Self> {
        check()?;
        if depth > MAX_DEPTH || budget.nodes >= budget.limit {
            return Err(invalid("recovery evidence tree exceeds bound"));
        }
        budget.nodes += 1;
        let stamp =
            entry(parent, &name)?.ok_or_else(|| invalid("recovery evidence disappeared"))?;
        if !stamp.safe() || stamp.size < 0 {
            return Err(invalid("unsafe recovery evidence inode"));
        }
        let file = File::from(openat(
            parent,
            name.as_os_str(),
            OFlag::O_RDONLY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK
                | if stamp.directory() {
                    OFlag::O_DIRECTORY
                } else {
                    OFlag::empty()
                },
            Mode::empty(),
        )?);
        check()?;
        if Stamp::of(&file)? != stamp || entry(parent, &name)? != Some(stamp) {
            return Err(invalid("recovery evidence changed during open"));
        }
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        let mut children = Vec::new();
        let mut child_names = BTreeSet::new();
        if stamp.directory() {
            child_names = names(&file, budget.limit - budget.nodes)?;
            for child in &child_names {
                children.push(Self::capture(
                    &file,
                    child.clone(),
                    depth + 1,
                    budget,
                    check,
                )?);
            }
        } else {
            let length =
                usize::try_from(stamp.size).map_err(|_| invalid("recovery evidence size"))?;
            if length as u64 > MAX_FILE_BYTES
                || length > MAX_TOTAL_BYTES.saturating_sub(budget.bytes)
            {
                return Err(invalid("recovery evidence bytes exceed bound"));
            }
            (&file).take(length as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() != length {
                return Err(invalid("recovery evidence size changed"));
            }
            budget.bytes += length;
        }
        let value = Self {
            name,
            file,
            stamp,
            sha256: Sha256::digest(bytes.as_slice()).into(),
            bytes,
            children,
            names: child_names,
        };
        value.verify(parent, check)?;
        Ok(value)
    }

    fn verify(&self, parent: &File, check: &impl Fn() -> io::Result<()>) -> io::Result<()> {
        check()?;
        if entry(parent, &self.name)? != Some(self.stamp) || Stamp::of(&self.file)? != self.stamp {
            return Err(invalid("captured recovery evidence namespace changed"));
        }
        if self.stamp.directory() {
            if names(&self.file, self.names.len())? != self.names {
                return Err(invalid("captured recovery evidence names changed"));
            }
            for child in &self.children {
                child.verify(&self.file, check)?;
            }
        } else {
            // Positional reads retain neither a shared cursor nor a path
            // reopen. Compare actual bytes, not only timestamps or a digest.
            let mut readback = zeroize::Zeroizing::new(vec![0; self.bytes.len() + 1]);
            let mut offset = 0;
            while offset < readback.len() {
                check()?;
                match self.file.read_at(&mut readback[offset..], offset as u64) {
                    Ok(0) => break,
                    Ok(count) => offset += count,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            readback.truncate(offset);
            if readback.as_slice() != self.bytes.as_slice()
                || <[u8; 32]>::from(Sha256::digest(readback.as_slice())) != self.sha256
            {
                return Err(invalid("captured recovery evidence bytes changed"));
            }
        }
        check()?;
        if entry(parent, &self.name)? != Some(self.stamp) || Stamp::of(&self.file)? != self.stamp {
            return Err(invalid("recovery evidence changed during verification"));
        }
        Ok(())
    }

    fn hash(&self, hash: &mut Sha256) {
        let name = self.name.as_os_str().as_bytes();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name);
        hash.update(self.stamp.dev.to_le_bytes());
        hash.update(self.stamp.ino.to_le_bytes());
        hash.update(self.stamp.mode.to_le_bytes());
        hash.update((self.bytes.len() as u64).to_le_bytes());
        hash.update(self.sha256);
        hash.update((self.children.len() as u64).to_le_bytes());
        for child in &self.children {
            child.hash(hash);
        }
    }
}

pub(super) struct EvidenceTree {
    root: File,
    root_identity: (u64, u64, u32, u32),
    entries: Vec<Option<Node>>,
    captured_names: Vec<&'static str>,
    pub(super) nodes: usize,
    pub(super) bytes: usize,
    pub(super) sha256: String,
}
impl EvidenceTree {
    pub(super) fn capture(root: &File, check: &impl Fn() -> io::Result<()>) -> io::Result<Self> {
        Self::capture_limit(root, check, MAX_NODES)
    }
    pub(super) fn capture_for_reconciliation(
        root: &File,
        check: &impl Fn() -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::capture_limit(root, check, 512)
    }
    fn capture_limit(
        root: &File,
        check: &impl Fn() -> io::Result<()>,
        limit: usize,
    ) -> io::Result<Self> {
        Self::capture_names(root, check, limit, &NAMES)
    }
    pub(super) fn capture_legacy_current(
        root: &File,
        check: &impl Fn() -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut names = NAMES.to_vec();
        names.extend([
            "module-legacy-locks-v1",
            "runtime-download-lineage-v1",
            "module-recovery-v1",
            crate::call_mode_restore_journal::ARCHIVE_NAME,
        ]);
        Self::capture_names(root, check, 512, &names)
    }
    fn capture_names(
        root: &File,
        check: &impl Fn() -> io::Result<()>,
        limit: usize,
        captured_names: &[&'static str],
    ) -> io::Result<Self> {
        check()?;
        let root_stamp = Stamp::of(root)?;
        if !root_stamp.directory() || !root_stamp.safe() || root_stamp.mode & 0o777 != 0o700 {
            return Err(invalid("recovery evidence requires a private held root"));
        }
        let held = File::from(openat(
            root,
            ".",
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        if Stamp::of(&held)? != root_stamp {
            return Err(invalid("recovery evidence root changed"));
        }
        let mut budget = Budget {
            limit,
            nodes: 0,
            bytes: 0,
        };
        let mut entries = Vec::with_capacity(captured_names.len());
        for &name in captured_names {
            check()?;
            entries.push(match entry(&held, OsStr::new(name))? {
                None => None,
                Some(_) => Some(Node::capture(&held, name.into(), 0, &mut budget, check)?),
            });
        }
        if Stamp::of(&held)? != root_stamp {
            return Err(invalid("recovery root changed during capture interval"));
        }
        let mut hash = Sha256::new();
        for (name, node) in captured_names.iter().zip(&entries) {
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update([u8::from(node.is_some())]);
            if let Some(node) = node {
                node.hash(&mut hash);
            }
        }
        let value = Self {
            root: held,
            root_identity: (
                root_stamp.dev,
                root_stamp.ino,
                root_stamp.uid,
                root_stamp.mode,
            ),
            entries,
            captured_names: captured_names.to_vec(),
            nodes: budget.nodes,
            bytes: budget.bytes,
            sha256: format!("{:x}", hash.finalize()),
        };
        value.verify(check)?;
        if Stamp::of(&value.root)? != root_stamp {
            return Err(invalid("recovery root changed before capture completed"));
        }
        Ok(value)
    }

    pub(super) fn verify(&self, check: &impl Fn() -> io::Result<()>) -> io::Result<()> {
        check()?;
        let stamp = Stamp::of(&self.root)?;
        if (stamp.dev, stamp.ino, stamp.uid, stamp.mode) != self.root_identity {
            return Err(invalid("captured recovery root metadata changed"));
        }
        for (name, node) in self.captured_names.iter().zip(&self.entries) {
            check()?;
            match node {
                Some(node) => node.verify(&self.root, check)?,
                None if entry(&self.root, OsStr::new(name))?.is_none() => {}
                None => return Err(invalid("previously absent recovery evidence appeared")),
            }
        }
        check()?;
        // The root may legitimately acquire unrelated reader stores between
        // observations. Within this one bounded interval, a detectable ABA of
        // an absent fixed slot must still invalidate the observation.
        if Stamp::of(&self.root)? != stamp {
            return Err(invalid(
                "recovery root changed during verification interval",
            ));
        }
        Ok(())
    }
}

impl EvidenceTree {
    /// The original parser validates schema, complete USB fields, voice fields
    /// and restore command consistency. Filenames and bounded bytes are data,
    /// never a claim that any historical operation finished.
    pub(super) fn legacy_current_backups(
        &self,
    ) -> io::Result<Vec<crate::call_mode_backups::NativeBackup>> {
        let Some(directory) = &self.entries[4] else {
            return Ok(Vec::new());
        };
        if !directory.stamp.directory() || directory.children.len() > 32 {
            return Err(invalid("legacy backup shape"));
        }
        directory
            .children
            .iter()
            .map(|node| {
                if !node.stamp.regular()
                    || !node
                        .name
                        .to_str()
                        .is_some_and(crate::call_mode_backups::valid_backup_id)
                {
                    return Err(invalid("legacy backup entry"));
                }
                crate::call_mode_backups::decode_legacy_current_backup(&node.bytes)
                    .map_err(|_| invalid("legacy backup data"))
            })
            .collect()
    }
    pub(super) fn has_legacy_current_data(&self) -> bool {
        self.entries[4].is_some() || self.entries[5].is_some()
    }
    pub(super) fn verify_legacy_current_runtime(&self) -> io::Result<()> {
        for index in [0, 6, 7] {
            if self.entries[index].is_some() {
                return Err(invalid("pending current baseline obligation"));
            }
        }
        for index in [1, 2, 3] {
            if let Some(node) = &self.entries[index] {
                if !node.stamp.regular() || !node.bytes.is_empty() || node.stamp.mode & 0o7133 != 0
                {
                    return Err(invalid("unsafe current baseline marker or lock"));
                }
            }
        }
        let Some(runtime) = &self.entries[5] else {
            return Ok(());
        };
        let manifest = crate::call_mode_runtime_manifest::manifest();
        if !runtime.stamp.directory() {
            return Err(invalid("legacy runtime shape"));
        }
        for node in &runtime.children {
            if node.name == ".download.lock" {
                if !node.stamp.regular() || !node.bytes.is_empty() || node.stamp.mode & 0o7133 != 0
                {
                    return Err(invalid("legacy runtime lock"));
                }
                continue;
            }
            if node.name != manifest.version.as_str()
                || !node.stamp.directory()
                || node.children.len() > manifest.files.len()
            {
                return Err(invalid("legacy runtime unknown entry"));
            }
            for artifact in &node.children {
                if !artifact.stamp.regular()
                    || !manifest
                        .files
                        .iter()
                        .any(|known| artifact.name == known.name.as_str())
                {
                    return Err(invalid("legacy runtime unknown artifact"));
                }
                // Known cache bytes may be absent, partial or corrupt. They
                // remain pinned data and are never executed by this reader.
                // The existing deploy/download path retains full hash checks.
            }
        }
        Ok(())
    }
}

// Persisted metadata contains no historical payload or voice secret. It binds
// the exact original inode and bytes independently of the mutable new ledger.
impl Node {
    fn manifest(&self) -> io::Result<serde_json::Value> {
        Ok(
            serde_json::json!({"name":self.name.to_str().ok_or_else(||invalid("non UTF8 evidence name"))?,
            "stamp":self.stamp,"sha256":format!("{:x}",Sha256::digest(self.bytes.as_slice())),
            "children":self.children.iter().map(Node::manifest).collect::<io::Result<Vec<_>>>()?}),
        )
    }
    fn verify_placement(
        &self,
        parent: &File,
        name: &str,
        check: &impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        check()?;
        let mut actual =
            entry(parent, OsStr::new(name))?.ok_or_else(|| invalid("retired evidence missing"))?;
        let mut held = Stamp::of(&self.file)?;
        // rename changes only this directory's ctime; every child remains
        // byte-for-byte and inode-for-inode pinned by ordinary verify.
        actual.ctime = self.stamp.ctime;
        held.ctime = self.stamp.ctime;
        if actual != self.stamp || held != self.stamp || !self.stamp.directory() {
            return Err(invalid("retired original identity changed"));
        }
        if names(&self.file, self.names.len())? != self.names {
            return Err(invalid("retired names changed"));
        }
        for child in &self.children {
            child.verify(&self.file, check)?;
        }
        check()
    }
}
impl EvidenceTree {
    pub(super) fn manifest(&self) -> io::Result<serde_json::Value> {
        let mut slots = serde_json::Map::new();
        for (name, node) in self.captured_names.iter().zip(&self.entries) {
            slots.insert(
                (*name).into(),
                match node {
                    Some(n) => n.manifest()?,
                    None => serde_json::Value::Null,
                },
            );
        }
        Ok(serde_json::json!({"schema":1,"root":self.root_identity,"slots":slots}))
    }
    pub(super) fn verify_retired(
        &self,
        archive: &File,
        check: &impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        check()?;
        let stamp = Stamp::of(&self.root)?;
        if (stamp.dev, stamp.ino, stamp.uid, stamp.mode) != self.root_identity {
            return Err(invalid("retirement root changed"));
        }
        for (name, node) in self.captured_names.iter().zip(&self.entries) {
            if matches!(*name, "call-mode-restore.active" | "voice-control") {
                if entry(&self.root, OsStr::new(name))?.is_some() {
                    return Err(invalid("retired active slot occupied"));
                }
                let archived = if *name == "call-mode-restore.active" {
                    "journal"
                } else {
                    "voice-control"
                };
                match node {
                    Some(n) => n.verify_placement(archive, archived, check)?,
                    None => {
                        if entry(archive, OsStr::new(archived))?.is_some() {
                            return Err(invalid("unexpected retired evidence"));
                        }
                    }
                }
            } else {
                match node {
                    Some(n) => n.verify(&self.root, check)?,
                    None => {
                        if entry(&self.root, OsStr::new(name))?.is_some() {
                            return Err(invalid("original absent slot appeared"));
                        }
                    }
                }
            }
        }
        check()
    }
    pub(super) fn persisted_manifest(
        root: &File,
        archive: Option<&File>,
    ) -> io::Result<serde_json::Value> {
        let original = Self::capture_for_reconciliation(root, &|| Ok(()))?;
        let mut manifest = original.manifest()?;
        if let Some(ar) = archive {
            let mut budget = Budget {
                limit: 512,
                nodes: original.nodes,
                bytes: original.bytes,
            };
            for (name, archived) in [
                ("call-mode-restore.active", "journal"),
                ("voice-control", "voice-control"),
            ] {
                let value = if entry(ar, OsStr::new(archived))?.is_some() {
                    let n = Node::capture(ar, archived.into(), 0, &mut budget, &|| Ok(()))?;
                    let mut value = n.manifest()?;
                    value["name"] = name.into();
                    value
                } else {
                    serde_json::Value::Null
                };
                manifest["slots"][name] = value;
            }
        }
        Ok(manifest)
    }
    pub(super) fn original_voice_present(&self) -> bool {
        self.entries[7].is_some()
    }
}

impl EvidenceTree {
    pub(super) fn transition_manifest(
        root: &File,
        archive: &File,
    ) -> io::Result<serde_json::Value> {
        let current = Self::capture_for_reconciliation(root, &|| Ok(()))?;
        let mut manifest = current.manifest()?;
        let mut budget = Budget {
            limit: 512,
            nodes: current.nodes,
            bytes: current.bytes,
        };
        for (name, archived) in [
            ("call-mode-restore.active", "journal"),
            ("voice-control", "voice-control"),
        ] {
            let live = entry(root, OsStr::new(name))?.is_some();
            let retired = entry(archive, OsStr::new(archived))?.is_some();
            if live && retired {
                return Err(invalid("duplicate original placement"));
            }
            if retired {
                let n = Node::capture(archive, archived.into(), 0, &mut budget, &|| Ok(()))?;
                let mut v = n.manifest()?;
                v["name"] = name.into();
                manifest["slots"][name] = v;
            }
        }
        Ok(manifest)
    }
}
impl EvidenceTree {
    pub(super) fn retired_slots_manifest(archive: &File) -> io::Result<serde_json::Value> {
        let mut slots = serde_json::Map::new();
        let mut budget = Budget {
            limit: 512,
            nodes: 0,
            bytes: 0,
        };
        for (name, archived) in [
            ("call-mode-restore.active", "journal"),
            ("voice-control", "voice-control"),
        ] {
            let value = if entry(archive, OsStr::new(archived))?.is_some() {
                let node = Node::capture(archive, archived.into(), 0, &mut budget, &|| Ok(()))?;
                let mut v = node.manifest()?;
                v["name"] = name.into();
                v
            } else {
                serde_json::Value::Null
            };
            slots.insert(name.into(), value);
        }
        Ok(serde_json::Value::Object(slots))
    }
}

// Internal metadata pin only; this grants no transport, Gate or mutation authority.
pub(super) fn pinned_regular_manifest(
    parent: &File,
    name: &str,
) -> io::Result<(File, serde_json::Value)> {
    let mut budget = Budget {
        limit: 1,
        nodes: 0,
        bytes: 0,
    };
    let node = Node::capture(parent, name.into(), 0, &mut budget, &|| Ok(()))?;
    if !node.stamp.regular() {
        return Err(invalid("expected regular backup lifecycle input"));
    }
    Ok((node.file.try_clone()?, node.manifest()?))
}

pub(super) fn pinned_empty_directory_manifest(
    parent: &File,
    name: &str,
) -> io::Result<(File, serde_json::Value)> {
    let mut budget = Budget {
        limit: 1,
        nodes: 0,
        bytes: 0,
    };
    let node = Node::capture(parent, name.into(), 0, &mut budget, &|| Ok(()))?;
    if !node.stamp.directory() || !node.children.is_empty() {
        return Err(invalid("expected empty staged backup parent"));
    }
    Ok((node.file.try_clone()?, node.manifest()?))
}

// Backup metadata writers historically accept safe owned 0755 roots. This
// bounded data-only reader preserves that contract without widening the
// private-root EvidenceTree used to mint physical reconciliation scopes.
pub(super) fn backup_lifecycle_snapshot(
    root: &File,
) -> io::Result<(serde_json::Value, usize, usize)> {
    let before = Stamp::of(root)?;
    if !before.directory()
        || before.uid != nix::unistd::geteuid().as_raw()
        || before.mode & 0o7022 != 0
    {
        return Err(invalid("unsafe backup lifecycle root"));
    }
    let mut budget = Budget {
        limit: MAX_NODES,
        nodes: 0,
        bytes: 0,
    };
    let node = if entry(root, OsStr::new("module-backups"))?.is_some() {
        Some(Node::capture(
            root,
            "module-backups".into(),
            0,
            &mut budget,
            &|| Ok(()),
        )?)
    } else {
        None
    };
    let after = Stamp::of(root)?;
    if (after.dev, after.ino, after.uid, after.gid, after.mode)
        != (before.dev, before.ino, before.uid, before.gid, before.mode)
    {
        return Err(invalid(
            "backup lifecycle root identity changed during capture",
        ));
    }
    let backup = node
        .as_ref()
        .map(Node::manifest)
        .transpose()?
        .unwrap_or(serde_json::Value::Null);
    Ok((
        serde_json::json!({"schema":1,"root":[before.dev,before.ino,before.uid,before.mode],"slots":{"module-backups":backup}}),
        budget.nodes,
        budget.bytes,
    ))
}
