//! Actual backup namespace transactions. No lifecycle record grants hardware
//! admission or creates recovery authority. Old backup inodes are never unlinked.
use super::{evidence, ledger, storage};
mod durability;
#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
use process_tests::{PublicationPoint, observe_publication};
macro_rules! boundary {
    ($point:ident,$generation:expr) => {
        #[cfg(test)]
        observe_publication(PublicationPoint::$point, $generation)?;
    };
}
use crate::daemon_lifetime::RootProof;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, fstatat, mkdirat};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{File, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

pub(super) const NAME: &str = "backup-lifecycle-v1";
const POLICY: &str = "backup-lifecycle-v1";
// Keep production recovery history at the full policy limit. Unit tests use a
// smaller cap so they can exercise the exact exhaustion path without running
// dozens of full, tamper-evident filesystem scans for each fixture.
#[cfg(not(test))]
const MAX_GENERATIONS: usize = 128;
#[cfg(test)]
const MAX_GENERATIONS: usize = 16;
const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_RECORD: usize = 512 * 1024;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_NODES: usize = 512;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupWriteKind {
    Create,
    Import,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Create,
    Import,
    Delete,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: u32,
    policy: String,
    generation: u64,
    previous: Option<String>,
    publication: Option<ledger::NormalJournalOrigin>,
    kind: Kind,
    id: String,
    payload: Option<Vec<u8>>,
    before: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    intent_sha256: String,
    entry: Value,
    parent: Option<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    intent_sha256: String,
    entry: Value,
    parent: Option<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    intent_sha256: String,
    entry: Value,
    after: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Abort {
    intent_sha256: String,
}

fn invalid() -> io::Error {
    io::Error::other("backup lifecycle remains pending or changed")
}
fn digest<T: Serialize>(v: &T) -> io::Result<String> {
    Ok(storage::digest(
        &serde_json::to_vec(v).map_err(io::Error::other)?,
    ))
}
fn names(dir: &File, limit: usize) -> io::Result<Vec<String>> {
    let mut values = proletarius_platform_fs::directory_names(dir, limit)?
        .into_iter()
        .map(|n| n.into_string().map_err(|_| invalid()))
        .collect::<io::Result<Vec<_>>>()?;
    values.sort();
    if values.windows(2).any(|w| w[0] == w[1]) {
        return Err(invalid());
    }
    Ok(values)
}
fn exists(parent: &File, name: &str) -> io::Result<bool> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(nix::errno::Errno::ENOENT) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn private_dir(parent: &File, name: &str, create: bool) -> io::Result<File> {
    if !exists(parent, name)? && create {
        match mkdirat(parent, name, Mode::S_IRWXU) {
            Ok(()) => parent.sync_all()?,
            Err(nix::errno::Errno::EEXIST) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let directory = storage::directory(parent, name)?;
    if create {
        parent.sync_all()?;
    }
    Ok(directory)
}
fn new_private_dir(parent: &File, name: &str) -> io::Result<File> {
    mkdirat(parent, name, Mode::S_IRWXU)?;
    parent.sync_all()?;
    storage::directory(parent, name)
}
fn public_dir(root: &File, create: bool) -> io::Result<Option<File>> {
    if !exists(root, "module-backups")? {
        if !create {
            return Ok(None);
        }
        mkdirat(root, "module-backups", Mode::S_IRWXU)?;
        root.sync_all()?;
    }
    let f = File::from(openat(
        root,
        "module-backups",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let m = f.metadata()?;
    let n = fstatat(root, "module-backups", AtFlags::AT_SYMLINK_NOFOLLOW)?;
    if !m.is_dir()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o7022 != 0
        || m.dev() != n.st_dev as u64
        || m.ino() != n.st_ino as u64
    {
        return Err(invalid());
    }
    Ok(Some(f))
}
fn read_record<T: for<'a> Deserialize<'a> + Serialize>(dir: &File, name: &str) -> io::Result<T> {
    let (file, pinned_entry) = evidence::pinned_regular_manifest(dir, name)?;
    let m = file.metadata()?;
    if m.mode() & 0o7777 != 0o600 || m.len() == 0 || m.len() > MAX_RECORD as u64 {
        return Err(invalid());
    }
    // Node capture has already read this open-file description to EOF;
    // dup/try_clone shares that cursor. Read immutable records by position,
    // without changing any retained descriptor's offset.
    let initial = file_stamp(&m);
    let mut raw = vec![0u8; m.len() as usize + 1];
    let mut count = 0;
    while count < raw.len() {
        let length = file.read_at(&mut raw[count..], count as u64)?;
        if length == 0 {
            break;
        }
        count += length;
    }
    raw.truncate(count);
    if raw.is_empty()
        || raw.len() > MAX_RECORD
        || raw.len() as u64 != m.len()
        || file_stamp(&file.metadata()?) != initial
    {
        return Err(invalid());
    }
    let v: T = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    if serde_json::to_vec(&v).map_err(io::Error::other)? != raw {
        return Err(invalid());
    }
    let (_, after) = evidence::pinned_regular_manifest(dir, name)?;
    if after != pinned_entry
        || after["sha256"] != storage::digest(&raw)
        || file_stamp(&file.metadata()?) != initial
    {
        return Err(invalid());
    }
    Ok(v)
}
fn write_record<T: Serialize>(dir: &File, name: &str, value: &T) -> io::Result<()> {
    let raw = serde_json::to_vec(value).map_err(io::Error::other)?;
    if raw.is_empty() || raw.len() > MAX_RECORD {
        return Err(invalid());
    }
    let mut f = File::from(openat(
        dir,
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )?);
    #[cfg(test)]
    super::finite_io_tests::write_all(dir, name, &mut f, &raw)?;
    #[cfg(not(test))]
    f.write_all(&raw)?;
    #[cfg(test)]
    {
        super::finite_io_tests::sync_all(super::finite_io_tests::SyncPoint::File, &f)?;
        super::finite_io_tests::sync_all(super::finite_io_tests::SyncPoint::RecordParent, dir)?;
    }
    #[cfg(not(test))]
    {
        f.sync_all()?;
        dir.sync_all()?;
    }
    Ok(())
}
fn manifest(root: &File) -> io::Result<Value> {
    Ok(evidence::backup_lifecycle_snapshot(root)?.0)
}
fn backups(v: &Value) -> &Value {
    &v["slots"]["module-backups"]
}
fn children(v: &Value) -> io::Result<Vec<Value>> {
    if v.is_null() {
        Ok(Vec::new())
    } else {
        v["children"].as_array().cloned().ok_or_else(invalid)
    }
}
fn target(v: &Value, id: &str) -> io::Result<Option<Value>> {
    let found = children(v)?
        .into_iter()
        .filter(|v| v["name"] == id)
        .collect::<Vec<_>>();
    if found.len() > 1 {
        return Err(invalid());
    }
    Ok(found.into_iter().next())
}
fn valid_entry(v: &Value, directory: bool) -> bool {
    let Some(object) = v.as_object() else {
        return false;
    };
    let Some(stamp) = v["stamp"].as_object() else {
        return false;
    };
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort();
    let mut stamp_keys = stamp.keys().map(String::as_str).collect::<Vec<_>>();
    stamp_keys.sort();
    if keys != vec!["children", "name", "sha256", "stamp"]
        || stamp_keys
            != vec![
                "ctime", "dev", "gid", "ino", "links", "mode", "mtime", "size", "uid",
            ]
        || !v["name"].is_string()
        || !v["children"].as_array().is_some_and(Vec::is_empty)
        || !v["sha256"].as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return false;
    }
    let Some(mode) = v["stamp"]["mode"].as_u64() else {
        return false;
    };
    let kind = if directory {
        nix::libc::S_IFDIR
    } else {
        nix::libc::S_IFREG
    } as u64;
    mode & nix::libc::S_IFMT as u64 == kind
        && mode & 0o7022 == 0
        && v["stamp"]["uid"].as_u64() == Some(nix::unistd::geteuid().as_raw() as u64)
        && ["dev", "ino", "gid", "size", "links"]
            .iter()
            .all(|k| v["stamp"][*k].as_u64().is_some())
        && (directory
            || (v["stamp"]["links"].as_u64() == Some(1)
                && v["stamp"]["size"]
                    .as_u64()
                    .is_some_and(|n| n > 0 && n <= MAX_PAYLOAD as u64)))
        && ["mtime", "ctime"].iter().all(|k| {
            v["stamp"][*k]
                .as_array()
                .is_some_and(|a| a.len() == 2 && a.iter().all(|n| n.as_i64().is_some()))
        })
}
fn same_file(a: &Value, b: &Value) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    a["name"] = Value::Null;
    b["name"] = Value::Null;
    // A genuine rename changes ctime. All other original inode/raw fields,
    // including size, mode, owner, nlink and mtime, must remain exact.
    a["stamp"]["ctime"] = Value::Null;
    b["stamp"]["ctime"] = Value::Null;
    a == b && a["stamp"]["links"].as_u64() == Some(1)
}
fn same_input(before: &Value, current: &Value) -> bool {
    before["root"] == current["root"] && backups(before) == backups(current)
}
fn same_parent(expected: &Value, actual: &File) -> io::Result<bool> {
    let m = actual.metadata()?;
    Ok(expected["stamp"]["dev"] == m.dev()
        && expected["stamp"]["ino"] == m.ino()
        && expected["stamp"]["mode"] == m.mode()
        && expected["stamp"]["uid"] == m.uid()
        && expected["stamp"]["gid"] == m.gid())
}
fn transition(
    before: &Value,
    after: &Value,
    id: &str,
    kind: Kind,
    entry: &Value,
) -> io::Result<()> {
    let old = children(before)?;
    let new = children(after)?;
    let old_other = old.iter().filter(|n| n["name"] != id).collect::<Vec<_>>();
    let new_other = new.iter().filter(|n| n["name"] != id).collect::<Vec<_>>();
    if old_other != new_other || after.is_null() {
        return Err(invalid());
    }
    if !before.is_null()
        && ["dev", "ino", "uid", "gid", "mode"]
            .iter()
            .any(|k| before["stamp"][*k] != after["stamp"][*k])
    {
        return Err(invalid());
    }
    let old_links = before["stamp"]["links"].as_u64();
    let links = after["stamp"]["links"].as_u64().ok_or_else(invalid)?;
    match kind {
        Kind::Delete => {
            if target(before, id)?.as_ref() != Some(entry) || target(after, id)?.is_some() {
                return Err(invalid());
            }
            if old_links.is_none_or(|n| links != n && n.checked_sub(1) != Some(links)) {
                return Err(invalid());
            }
        }
        _ => {
            if target(before, id)?.is_some()
                || !target(after, id)?.is_some_and(|n| same_file(&n, entry))
            {
                return Err(invalid());
            }
            if old_links.is_some_and(|n| links != n && n.checked_add(1) != Some(links)) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

struct Transaction {
    directory: File,
    intent: Intent,
    ready: Option<Ready>,
    prepare: Option<Prepare>,
    commit: Option<Commit>,
    abort: Option<Abort>,
}
struct Inventory {
    transactions: Vec<Transaction>,
    nodes: usize,
    bytes: usize,
}
fn namespace(root: &File) -> io::Result<Option<File>> {
    let Some(parent) = storage::optional_dir(root, storage::NAME)? else {
        return Ok(None);
    };
    storage::optional_dir(&parent, NAME)
}
fn lock_file(ns: &File, create: bool) -> io::Result<File> {
    let flags = OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mode = Mode::S_IRUSR | Mode::S_IWUSR;
    // Only the exclusive creator may allocate this name. An existing name is
    // reopened without creation, then checked against the actual held inode.
    // ENOENT and all errors other than EEXIST are propagated without retry.
    let opened = if create {
        match openat(
            ns,
            "inventory.lock",
            flags | OFlag::O_CREAT | OFlag::O_EXCL,
            mode,
        ) {
            Ok(file) => Ok(file),
            Err(nix::errno::Errno::EEXIST) => openat(ns, "inventory.lock", flags, mode),
            Err(error) => Err(error),
        }
    } else {
        openat(ns, "inventory.lock", flags, mode)
    };
    let f = File::from(opened?);
    let m = f.metadata()?;
    let n = fstatat(ns, "inventory.lock", AtFlags::AT_SYMLINK_NOFOLLOW)?;
    if !m.is_file()
        || m.uid() != nix::unistd::geteuid().as_raw()
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.len() != 0
        || m.dev() != n.st_dev as u64
        || m.ino() != n.st_ino as u64
    {
        return Err(invalid());
    }
    Ok(f)
}
fn scan(root: &File) -> io::Result<Option<Inventory>> {
    let Some(ns) = namespace(root)? else {
        return Ok(None);
    };
    let all = names(&ns, MAX_GENERATIONS + 1)?;
    let original_names = all.clone();
    let namespace_stamp = (ns.metadata()?.dev(), ns.metadata()?.ino());
    if all.is_empty() {
        return Ok(Some(Inventory {
            transactions: Vec::new(),
            nodes: 1,
            bytes: 0,
        }));
    }
    let _lock = lock_file(&ns, false)?;
    let meta = root.metadata()?;
    let root_identity = serde_json::json!([meta.dev(), meta.ino(), meta.uid(), meta.mode()]);
    let generation_count = all.len() - 1;
    let mut out = Inventory {
        transactions: Vec::new(),
        nodes: 2,
        bytes: 0,
    };
    let mut previous = None;
    let mut last_after: Option<Value> = None;
    for name in all.into_iter().filter(|n| n != "inventory.lock") {
        let generation = out.transactions.len() + 1;
        if name != format!("{generation:06}") {
            return Err(invalid());
        }
        let dir = private_dir(&ns, &name, false)?;
        out.nodes += 1;
        let entries = names(&dir, 8)?;
        if !entries.iter().any(|n| n == "intent.json")
            || entries.iter().any(|n| {
                !matches!(
                    n.as_str(),
                    "intent.json"
                        | "stage"
                        | "archive"
                        | "ready.json"
                        | "prepare.json"
                        | "commit.json"
                        | "abort.json"
                        | "public-parent"
                )
            })
        {
            return Err(invalid());
        }
        for n in &entries {
            if n == "public-parent" {
                evidence::pinned_empty_directory_manifest(&dir, n)?;
                out.nodes += 1;
                continue;
            }
            let (file, _) = evidence::pinned_regular_manifest(&dir, n)?;
            out.nodes += 1;
            out.bytes = out
                .bytes
                .checked_add(file.metadata()?.len() as usize)
                .ok_or_else(invalid)?;
        }
        if out.nodes > MAX_NODES || out.bytes > MAX_BYTES {
            return Err(invalid());
        }
        let intent: Intent = read_record(&dir, "intent.json")?;
        if intent.schema != 1
            || intent.policy != POLICY
            || intent.generation != generation as u64
            || intent.previous != previous
            || intent.before["root"] != root_identity
            || !crate::call_mode_backups::valid_backup_id(&intent.id)
            || last_after
                .as_ref()
                .is_some_and(|v| backups(&intent.before) != v)
            || match intent.kind {
                Kind::Delete => intent.payload.is_some(),
                _ => intent
                    .payload
                    .as_ref()
                    .is_none_or(|v| v.is_empty() || v.len() > MAX_PAYLOAD),
            }
        {
            return Err(invalid());
        }
        let hash = digest(&intent)?;
        let ready = if entries.iter().any(|n| n == "ready.json") {
            Some(read_record::<Ready>(&dir, "ready.json")?)
        } else {
            None
        };
        let prepare = if entries.iter().any(|n| n == "prepare.json") {
            Some(read_record::<Prepare>(&dir, "prepare.json")?)
        } else {
            None
        };
        let commit = if entries.iter().any(|n| n == "commit.json") {
            Some(read_record::<Commit>(&dir, "commit.json")?)
        } else {
            None
        };
        let abort = if entries.iter().any(|n| n == "abort.json") {
            Some(read_record::<Abort>(&dir, "abort.json")?)
        } else {
            None
        };
        if ready.as_ref().is_some_and(|v| v.intent_sha256 != hash)
            || prepare.as_ref().is_some_and(|v| v.intent_sha256 != hash)
            || commit.as_ref().is_some_and(|v| v.intent_sha256 != hash)
            || abort.as_ref().is_some_and(|v| v.intent_sha256 != hash)
            || (abort.is_some()
                && (prepare.is_some()
                    || ready.is_some()
                    || entries
                        .iter()
                        .any(|n| matches!(n.as_str(), "stage" | "archive" | "public-parent"))))
            || (intent.kind == Kind::Delete && ready.is_some())
        {
            return Err(invalid());
        }
        if let Some(r) = &ready {
            let payload = intent.payload.as_ref().ok_or_else(invalid)?;
            if !valid_entry(&r.entry, false)
                || r.entry["name"] != "stage"
                || r.entry["sha256"] != storage::digest(payload)
                || r.entry["stamp"]["size"].as_u64() != Some(payload.len() as u64)
                || r.entry["stamp"]["mode"]
                    .as_u64()
                    .is_none_or(|m| m & 0o7777 != 0o600)
                || r.parent.is_some() != backups(&intent.before).is_null()
                || r.parent
                    .as_ref()
                    .is_some_and(|p| !valid_entry(p, true) || p["name"] != "public-parent")
            {
                return Err(invalid());
            }
        }
        if let Some(p) = &prepare {
            if !valid_entry(&p.entry, false) {
                return Err(invalid());
            }
            if intent.kind == Kind::Delete {
                if target(backups(&intent.before), &intent.id)?.as_ref() != Some(&p.entry) {
                    return Err(invalid());
                }
            } else if ready
                .as_ref()
                .is_none_or(|r| r.entry != p.entry || r.parent != p.parent)
            {
                return Err(invalid());
            }
        }
        if let Some(p) = &prepare {
            if (p.parent.is_some())
                != (intent.kind != Kind::Delete && backups(&intent.before).is_null())
            {
                return Err(invalid());
            }
        }
        if let Some(c) = &commit {
            let p = prepare.as_ref().ok_or_else(invalid)?;
            if !valid_entry(&c.entry, false)
                || !same_file(&p.entry, &c.entry)
                || c.entry["name"]
                    != if intent.kind == Kind::Delete {
                        "archive"
                    } else {
                        &intent.id
                    }
            {
                return Err(invalid());
            }
            transition(
                backups(&intent.before),
                &c.after,
                &intent.id,
                intent.kind,
                &p.entry,
            )?;
            if entries
                .iter()
                .any(|n| matches!(n.as_str(), "stage" | "public-parent"))
            {
                return Err(invalid());
            }
            if intent.kind == Kind::Delete {
                let (_, actual) = evidence::pinned_regular_manifest(&dir, "archive")?;
                if actual != c.entry {
                    return Err(invalid());
                }
            } else if entries.iter().any(|n| n == "archive") {
                return Err(invalid());
            }
            previous = Some(digest(c)?);
            last_after = Some(c.after.clone());
        } else if let Some(a) = &abort {
            previous = Some(digest(a)?);
            last_after = Some(backups(&intent.before).clone());
        } else if generation != generation_count {
            return Err(invalid());
        }
        let named = private_dir(&ns, &name, false)?;
        if (named.metadata()?.dev(), named.metadata()?.ino())
            != (dir.metadata()?.dev(), dir.metadata()?.ino())
        {
            return Err(invalid());
        }
        out.transactions.push(Transaction {
            directory: dir,
            intent,
            ready,
            prepare,
            commit,
            abort,
        });
    }
    if names(&ns, MAX_GENERATIONS + 1)? != original_names {
        return Err(invalid());
    }
    let named = namespace(root)?.ok_or_else(invalid)?;
    if (named.metadata()?.dev(), named.metadata()?.ino()) != namespace_stamp {
        return Err(invalid());
    }
    let after = root.metadata()?;
    if root_identity != serde_json::json!([after.dev(), after.ino(), after.uid(), after.mode()]) {
        return Err(invalid());
    }
    Ok(Some(out))
}

/// Parser/budget only; it never mints a permit or executes a filesystem mutation.
pub(super) fn namespace_anchor(root: &File) -> io::Result<Value> {
    let ns = namespace(root)?.ok_or_else(invalid)?;
    let m = ns.metadata()?;
    Ok(
        serde_json::json!({"policy":POLICY,"dev":m.dev(),"ino":m.ino(),"uid":m.uid(),"mode":m.mode()}),
    )
}
pub(super) fn validate_namespace_anchor(root: &File, value: &Value) -> io::Result<()> {
    if &namespace_anchor(root)? != value {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn budget_usage(root: &File) -> io::Result<(usize, usize)> {
    Ok(scan(root)?.map(|v| (v.nodes, v.bytes)).unwrap_or((0, 0)))
}
pub(super) fn is_empty(root: &File) -> io::Result<bool> {
    Ok(scan(root)?.is_none_or(|v| v.transactions.is_empty()))
}
/// Only fully settled data transactions with no physical publication qualify
/// as input to a new physical current-state check. No repair or fsync is done.
pub(crate) fn legacy_current_data_only(root: &File) -> io::Result<bool> {
    let Some(parent) = storage::optional_dir(root, storage::NAME)? else { return Ok(false); };
    if names(&parent, 2)? != vec![NAME.to_owned()] { return Err(invalid()); }
    let inventory = scan(root)?.ok_or_else(invalid)?;
    if inventory.transactions.is_empty() || inventory.transactions.iter().any(|t|
        t.intent.publication.is_some() || (t.commit.is_none() && t.abort.is_none())) {
        return Err(invalid());
    }
    verify_finished(root, &inventory)?;
    Ok(true)
}

pub(super) fn complete(root: &File) -> io::Result<bool> {
    let Some(v) = scan(root)? else {
        return Ok(true);
    };
    if v.transactions
        .iter()
        .any(|t| t.commit.is_none() && t.abort.is_none())
    {
        return Ok(false);
    }
    verify_finished(root, &v)?;
    durability::resync_finished(root, &v)?;
    Ok(true)
}

/// Data provenance for this one slot, never a device or recovery permission.
/// Only this reader can construct it; the startup reader must revalidate it
/// after inspecting its other, independently blocking evidence slots.
pub(crate) struct FreshBackupCoverage {
    state: Value,
}
impl FreshBackupCoverage {
    pub(crate) fn validate(&self, root: &File) -> io::Result<()> {
        if fresh_backup_state(root)?.as_ref() != Some(&self.state) {
            return Err(invalid());
        }
        Ok(())
    }
}
pub(crate) fn verified_fresh_backup_slot(root: &File) -> io::Result<Option<FreshBackupCoverage>> {
    Ok(fresh_backup_state(root)?.map(|state| FreshBackupCoverage { state }))
}
fn fresh_backup_state(root: &File) -> io::Result<Option<Value>> {
    let Some(ns) = namespace(root)? else {
        return Ok(None);
    };
    let parent = storage::optional_dir(root, storage::NAME)?.ok_or_else(invalid)?;
    let parent_before = file_stamp(&parent.metadata()?);
    let ns_before = file_stamp(&ns.metadata()?);
    let inventory = scan(root)?.ok_or_else(invalid)?;
    let Some(first) = inventory.transactions.first() else {
        return Ok(None);
    };
    // A pre-existing directory, even empty, is a legacy baseline. All physical
    // publications and normal origins remain the reconciliation reader's job.
    if !backups(&first.intent.before).is_null()
        || inventory
            .transactions
            .iter()
            .any(|t| t.intent.publication.is_some())
        || storage::lifecycle_origin_attempts(root)
            .map_err(|_| invalid())?
            .is_some_and(|attempts| !attempts.is_empty())
        || !storage::origins(root).map_err(|_| invalid())?.is_empty()
    {
        return Ok(None);
    }
    if inventory
        .transactions
        .iter()
        .any(|t| t.commit.is_none() && t.abort.is_none())
    {
        return Err(invalid());
    }
    verify_finished(root, &inventory)?;
    durability::resync_finished(root, &inventory)?;
    let current = manifest(root)?;
    let mut records = Vec::with_capacity(inventory.transactions.len());
    for t in &inventory.transactions {
        let before = file_stamp(&t.directory.metadata()?);
        let mut files = Vec::new();
        let expected = [
            ("intent.json", Some(digest(&t.intent)?)),
            ("ready.json", t.ready.as_ref().map(digest).transpose()?),
            ("prepare.json", t.prepare.as_ref().map(digest).transpose()?),
            ("commit.json", t.commit.as_ref().map(digest).transpose()?),
            ("abort.json", t.abort.as_ref().map(digest).transpose()?),
        ];
        for (name, hash) in expected {
            if let Some(hash) = hash {
                let (_, actual) = evidence::pinned_regular_manifest(&t.directory, name)?;
                if actual["sha256"] != hash {
                    return Err(invalid());
                }
                files.push(actual);
            }
        }
        let named = private_dir(&ns, &format!("{:06}", t.intent.generation), false)?;
        if file_stamp(&named.metadata()?) != before
            || file_stamp(&t.directory.metadata()?) != before
        {
            return Err(invalid());
        }
        records.push(serde_json::json!({"directory":before,"records":files}));
    }
    let (_, lock) = evidence::pinned_regular_manifest(&ns, "inventory.lock")?;
    if file_stamp(&parent.metadata()?) != parent_before
        || file_stamp(&ns.metadata()?) != ns_before
        || file_stamp(&namespace(root)?.ok_or_else(invalid)?.metadata()?) != ns_before
        || file_stamp(
            &storage::optional_dir(root, storage::NAME)?
                .ok_or_else(invalid)?
                .metadata()?,
        ) != parent_before
        || manifest(root)? != current
    {
        return Err(invalid());
    }
    Ok(Some(
        serde_json::json!({"parent":parent_before,"namespace":ns_before,
        "inventory_lock":lock,"current":current,"transactions":records}),
    ))
}
fn verify_finished(root: &File, v: &Inventory) -> io::Result<()> {
    let mut head = None;
    let mut origins = Vec::new();
    for t in &v.transactions {
        if let Some(origin) = t.intent.publication.as_ref() {
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
        head = Some(
            t.commit
                .as_ref()
                .map(|c| &c.after)
                .unwrap_or_else(|| backups(&t.intent.before)),
        );
    }
    ledger::verify_backup_lifecycle_origins(root, &origins)?;
    if let Some(expected) = head {
        if backups(&manifest(root)?) != expected {
            return Err(invalid());
        }
    }
    Ok(())
}
/// All historical and current placements are validated first. The ledger may
/// choose only an exact checkpoint that its original manifest/origins accepts.
pub(super) fn verified_checkpoints(root: &File) -> io::Result<Option<Vec<Value>>> {
    let Some(v) = scan(root)? else {
        return Ok(None);
    };
    if v.transactions
        .iter()
        .any(|t| t.commit.is_none() && t.abort.is_none())
    {
        return Err(invalid());
    }
    verify_finished(root, &v)?;
    durability::resync_finished(root, &v)?;
    let mut values = Vec::new();
    if let Some(first) = v.transactions.first() {
        values.push(backups(&first.intent.before).clone());
    }
    for t in &v.transactions {
        values.push(
            t.commit
                .as_ref()
                .map(|c| c.after.clone())
                .unwrap_or_else(|| backups(&t.intent.before).clone()),
        );
    }
    Ok(Some(values))
}

struct Held {
    root: PathBuf,
    proof: RootProof,
    directory: File,
    namespace: File,
    lock: File,
}
impl Held {
    fn acquire(root: &Path, proof: &RootProof, create: bool) -> io::Result<Option<Self>> {
        proof
            .with_root(root, |held| -> io::Result<Option<Self>> {
                if !create && namespace(held)?.is_none() {
                    return Ok(None);
                }
                let parent = private_dir(held, storage::NAME, create)?;
                let ns = private_dir(&parent, NAME, create)?;
                let lock = lock_file(&ns, create)?;
                ns.sync_all()?;
                held.sync_all()?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    match lock.try_lock() {
                        Ok(()) => break,
                        Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(std::time::Duration::from_millis(5))
                        }
                        Err(TryLockError::WouldBlock) => {
                            return Err(io::Error::from(io::ErrorKind::WouldBlock));
                        }
                        Err(TryLockError::Error(e)) => return Err(e),
                    }
                }
                let value = Self {
                    root: root.into(),
                    proof: proof.clone(),
                    directory: held.try_clone()?,
                    namespace: ns,
                    lock,
                };
                value.check()?;
                Ok(Some(value))
            })
            .map_err(|_| invalid())?
    }
    fn check(&self) -> io::Result<()> {
        self.proof
            .with_root(&self.root, |root| -> io::Result<()> {
                let live = namespace(root)?.ok_or_else(invalid)?;
                let current = live.metadata()?;
                let held = self.namespace.metadata()?;
                if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
                    return Err(invalid());
                }
                let actual = lock_file(&live, false)?.metadata()?;
                let pinned = self.lock.metadata()?;
                if (actual.dev(), actual.ino()) != (pinned.dev(), pinned.ino()) {
                    return Err(invalid());
                }
                Ok(())
            })
            .map_err(|_| invalid())?
    }
    fn release(self) -> io::Result<()> {
        let before = self.check();
        let unlock = self.lock.unlock();
        let after = self.check();
        before.and(unlock).and(after)
    }
}

type FileStamp = (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64);
fn file_stamp(m: &std::fs::Metadata) -> FileStamp {
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
pub(crate) struct BackupDeleteSelection {
    file: File,
    stamp: FileStamp,
    sha256: String,
}
impl BackupDeleteSelection {
    pub(crate) fn capture(
        file: &File,
        initial: &std::fs::Metadata,
        payload: &[u8],
    ) -> io::Result<Self> {
        let expected = file_stamp(initial);
        if !initial.is_file()
            || initial.nlink() != 1
            || initial.uid() != nix::unistd::geteuid().as_raw()
            || initial.mode() & 0o7022 != 0
            || payload.is_empty()
            || payload.len() > MAX_PAYLOAD
            || payload.len() as u64 != initial.len()
            || file_stamp(&file.metadata()?) != expected
        {
            return Err(invalid());
        }
        let mut raw = vec![0u8; payload.len() + 1];
        let mut count = 0;
        while count < raw.len() {
            let n = file.read_at(&mut raw[count..], count as u64)?;
            if n == 0 {
                break;
            }
            count += n;
        }
        raw.truncate(count);
        if raw != payload || file_stamp(&file.metadata()?) != expected {
            return Err(invalid());
        }
        Ok(Self {
            file: file.try_clone()?,
            stamp: expected,
            sha256: storage::digest(&raw),
        })
    }
    fn verify(&self, parent: &File, id: &str) -> io::Result<File> {
        if file_stamp(&self.file.metadata()?) != self.stamp {
            return Err(invalid());
        }
        let (file, node) = evidence::pinned_regular_manifest(parent, id)?;
        if file_stamp(&file.metadata()?) != self.stamp || node["sha256"] != self.sha256 {
            return Err(invalid());
        }
        Ok(self.file.try_clone()?)
    }
}
fn check_transaction(held: &Held, dir: &File, generation: u64) -> io::Result<()> {
    held.check()?;
    let named = private_dir(&held.namespace, &format!("{generation:06}"), false)?;
    let a = named.metadata()?;
    let b = dir.metadata()?;
    if (a.dev(), a.ino(), a.mode(), a.uid(), a.gid())
        != (b.dev(), b.ino(), b.mode(), b.uid(), b.gid())
    {
        return Err(invalid());
    }
    held.check()
}
fn transaction_record<T: Serialize>(
    held: &Held,
    dir: &File,
    generation: u64,
    name: &str,
    value: &T,
) -> io::Result<()> {
    check_transaction(held, dir, generation)?;
    #[cfg(test)]
    let _finite_io_context = super::finite_io_tests::ContextGuard::backup(
        &held.directory,
        dir,
        generation,
        match name {
            "ready.json" => super::finite_io_tests::Record::Ready,
            "commit.json" => super::finite_io_tests::Record::Commit,
            _ => super::finite_io_tests::Record::Other,
        },
        super::finite_io_tests::Mode::Mutation,
    )?;
    write_record(dir, name, value)?;
    check_transaction(held, dir, generation)
}
pub(crate) struct BackupLifecyclePermit {
    held: Held,
    transaction: File,
    intent: Intent,
    pinned: Option<File>,
}
pub(crate) fn prepare_create(
    root: &Path,
    proof: &RootProof,
    id: &str,
    kind: BackupWriteKind,
    payload: &[u8],
) -> io::Result<BackupLifecyclePermit> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    begin(
        root,
        proof,
        id,
        match kind {
            BackupWriteKind::Create => Kind::Create,
            BackupWriteKind::Import => Kind::Import,
        },
        Some(payload.to_vec()),
        None,
    )
}
pub(crate) fn prepare_delete_selected(
    root: &Path,
    proof: &RootProof,
    id: &str,
    selection: &BackupDeleteSelection,
) -> io::Result<BackupLifecyclePermit> {
    begin(root, proof, id, Kind::Delete, None, Some(selection))
}
fn reserve_live_growth(root: &File, kind: Kind, payload: usize) -> io::Result<()> {
    if kind == Kind::Delete {
        return Ok(());
    }
    let (nodes, bytes) = if root.metadata()?.mode() & 0o7777 == 0o700 {
        let tree = evidence::EvidenceTree::capture(root, &|| Ok(()))?;
        (tree.nodes, tree.bytes)
    } else {
        let (_, nodes, bytes) = evidence::backup_lifecycle_snapshot(root)?;
        (nodes, bytes)
    };
    let added = if public_dir(root, false)?.is_some() {
        1
    } else {
        2
    };
    if nodes.checked_add(added).is_none_or(|n| n > 256)
        || bytes
            .checked_add(payload)
            .is_none_or(|n| n > 8 * 1024 * 1024)
    {
        return Err(io::Error::other(
            "backup lifecycle live evidence capacity exhausted",
        ));
    }
    Ok(())
}
fn begin(
    root: &Path,
    proof: &RootProof,
    id: &str,
    kind: Kind,
    payload: Option<Vec<u8>>,
    selection: Option<&BackupDeleteSelection>,
) -> io::Result<BackupLifecyclePermit> {
    if !crate::call_mode_backups::valid_backup_id(id)
        || (kind == Kind::Delete) != selection.is_some()
    {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    // Side-effect-free target existence checks preserve collision/absence
    // responses without racing a whole inventory read outside its real flock.
    proof
        .with_root(root, |directory| -> io::Result<()> {
            let public = public_dir(directory, false)?;
            let occupied = public
                .as_ref()
                .map(|dir| exists(dir, id))
                .transpose()?
                .unwrap_or(false);
            match (kind, occupied) {
                (Kind::Delete, false) => return Err(io::Error::from(io::ErrorKind::NotFound)),
                (Kind::Create | Kind::Import, true) => {
                    return Err(io::Error::from(io::ErrorKind::AlreadyExists));
                }
                _ => {}
            }
            if namespace(directory)?.is_some() {
                return Ok(());
            }
            // The first allocation must reserve even its namespace/lock overhead
            // without writing anything. A concurrent genuine allocator may make
            // the namespace present while this snapshot is being captured; then
            // use that exact namespace and serialize before the authoritative read.
            let preflight = (|| {
                let before = manifest(directory)?;
                let encoded_before = serde_json::to_vec(&before).map_err(io::Error::other)?.len();
                let encoded_payload = serde_json::to_vec(&payload)
                    .map_err(io::Error::other)?
                    .len();
                if encoded_before
                    .checked_add(encoded_payload)
                    .and_then(|v| v.checked_add(2048))
                    .is_none_or(|v| v > MAX_RECORD)
                {
                    return Err(io::Error::other(
                        "backup lifecycle individual record capacity exhausted",
                    ));
                }
                let reserve =
                    payload.as_ref().map_or(0, |v| v.len() * 6) + encoded_before * 3 + 16 * 1024;
                storage::reserve_lifecycle_budget(directory, 12, reserve)?;
                ledger::reserve_backup_lifecycle_original_budget(directory, 12, reserve)?;
                reserve_live_growth(directory, kind, payload.as_ref().map_or(0, Vec::len))
            })();
            if preflight.is_err() {
                // Another writer can have allocated the safe outer namespace
                // while its nested namespace is not yet visible. Its mkdir
                // changes the full-root capacity snapshot. Wait only for an
                // actual, strictly reopened nested namespace, then take its
                // inventory flock and redo every authoritative capacity check.
                // A failed reservation never authorizes us to allocate one.
                let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    if namespace(directory)?.is_some() {
                        return Ok(());
                    }
                    if std::time::Instant::now() >= until
                        || storage::optional_dir(directory, storage::NAME)?.is_none()
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            preflight
        })
        .map_err(|_| invalid())??;
    let held = Held::acquire(root, proof, true)?.ok_or_else(invalid)?;
    let result = (|| {
        if !complete(&held.directory)? {
            return Err(invalid());
        }
        let publication = ledger::capture_normal_journal_origin(&held.directory)?;
        let before = manifest(&held.directory)?;
        let old = target(backups(&before), id)?;
        match (kind, old.is_some()) {
            (Kind::Delete, false) => return Err(io::Error::from(io::ErrorKind::NotFound)),
            (Kind::Create | Kind::Import, true) => {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            _ => {}
        }
        let prior = scan(&held.directory)?;
        let number = prior.as_ref().map_or(0, |v| v.transactions.len());
        let previous = prior
            .as_ref()
            .and_then(|v| v.transactions.last())
            .map(|v| {
                if let Some(c) = &v.commit {
                    digest(c)
                } else {
                    digest(v.abort.as_ref().expect("complete transaction"))
                }
            })
            .transpose()?;
        // Reserve a conservative complete generation before any Intent/temp or
        // public backup mutation. Capacity failure leaves the old head usable.
        let reserve = payload.as_ref().map_or(0, |v| v.len() * 6)
            + serde_json::to_vec(&before).map_err(io::Error::other)?.len() * 3
            + 16 * 1024;
        let (nodes, bytes) = prior.as_ref().map(|v| (v.nodes, v.bytes)).unwrap_or((2, 0));
        if number >= MAX_GENERATIONS
            || nodes + 9 > MAX_NODES
            || bytes.checked_add(reserve).is_none_or(|v| v > MAX_BYTES)
        {
            return Err(io::Error::other(
                "backup lifecycle durable capacity exhausted",
            ));
        }
        storage::reserve_lifecycle_budget(&held.directory, 12, reserve)?;
        ledger::reserve_backup_lifecycle_original_budget(&held.directory, 12, reserve)?;
        reserve_live_growth(&held.directory, kind, payload.as_ref().map_or(0, Vec::len))?;
        let intent = Intent {
            schema: 1,
            policy: POLICY.into(),
            generation: number as u64 + 1,
            previous,
            publication,
            kind,
            id: id.into(),
            payload,
            before,
        };
        if serde_json::to_vec(&intent).map_err(io::Error::other)?.len() > MAX_RECORD {
            return Err(invalid());
        }
        let pinned = if kind == Kind::Delete {
            let dir = public_dir(&held.directory, false)?.ok_or_else(invalid)?;
            Some(selection.ok_or_else(invalid)?.verify(&dir, id)?)
        } else {
            None
        };
        // Anchor allocation itself is reserved above and remains append-only;
        // an empty anchored namespace is a complete, harmless bootstrap state.
        ledger::anchor_backup_lifecycle(&held.directory, intent.publication.as_ref())?;
        boundary!(BeforeGeneration, intent.generation);
        let dir = new_private_dir(&held.namespace, &format!("{:06}", intent.generation))?;
        boundary!(AfterGeneration, intent.generation);
        boundary!(BeforeIntent, intent.generation);
        transaction_record(&held, &dir, intent.generation, "intent.json", &intent)?;
        boundary!(AfterIntent, intent.generation);
        held.check()?;
        Ok((dir, intent, pinned))
    })();
    match result {
        Ok((transaction, intent, pinned)) => Ok(BackupLifecyclePermit {
            held,
            transaction,
            intent,
            pinned,
        }),
        Err(e) => {
            let release = held.release();
            match release {
                Ok(()) => Err(e),
                Err(e) => Err(e),
            }
        }
    }
}
impl BackupLifecyclePermit {
    pub(crate) fn commit(mut self) -> io::Result<()> {
        let result = self.execute();
        let release = self.held.release();
        match (result, release) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(e), Ok(())) => Err(e),
            (_, Err(e)) => Err(e),
        }
    }
    fn execute(&mut self) -> io::Result<()> {
        check_transaction(&self.held, &self.transaction, self.intent.generation)?;
        if !same_input(&self.intent.before, &manifest(&self.held.directory)?) {
            return Err(invalid());
        }
        let hash = digest(&self.intent)?;
        let entry = if self.intent.kind == Kind::Delete {
            let dir = public_dir(&self.held.directory, false)?.ok_or_else(invalid)?;
            let (file, node) = evidence::pinned_regular_manifest(&dir, &self.intent.id)?;
            if file.metadata()?.ino() != self.pinned.as_ref().ok_or_else(invalid)?.metadata()?.ino()
                || target(backups(&self.intent.before), &self.intent.id)?.as_ref() != Some(&node)
            {
                return Err(invalid());
            }
            node
        } else {
            let raw = self.intent.payload.as_ref().ok_or_else(invalid)?;
            boundary!(BeforeTemp, self.intent.generation);
            let mut f = File::from(openat(
                &self.transaction,
                "stage",
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::S_IRUSR | Mode::S_IWUSR,
            )?);
            boundary!(AfterTempAllocated, self.intent.generation);
            #[cfg(test)]
            let _finite_io_context = super::finite_io_tests::ContextGuard::backup(
                &self.held.directory,
                &self.transaction,
                self.intent.generation,
                super::finite_io_tests::Record::Stage,
                super::finite_io_tests::Mode::Mutation,
            )?;
            #[cfg(test)]
            super::finite_io_tests::write_all(&self.transaction, "stage", &mut f, raw)?;
            #[cfg(not(test))]
            f.write_all(raw)?;
            boundary!(AfterTempWrite, self.intent.generation);
            #[cfg(test)]
            super::finite_io_tests::sync_all(super::finite_io_tests::SyncPoint::File, &f)?;
            #[cfg(not(test))]
            f.sync_all()?;
            self.transaction.sync_all()?;
            boundary!(AfterTempSync, self.intent.generation);
            let (file, node) = evidence::pinned_regular_manifest(&self.transaction, "stage")?;
            if node["sha256"] != storage::digest(raw) {
                return Err(invalid());
            }
            self.pinned = Some(file);
            let parent = if backups(&self.intent.before).is_null() {
                let _parent = new_private_dir(&self.transaction, "public-parent")?;
                Some(
                    evidence::pinned_empty_directory_manifest(&self.transaction, "public-parent")?
                        .1,
                )
            } else {
                None
            };
            transaction_record(
                &self.held,
                &self.transaction,
                self.intent.generation,
                "ready.json",
                &Ready {
                    intent_sha256: hash.clone(),
                    entry: node.clone(),
                    parent,
                },
            )?;
            boundary!(AfterReady, self.intent.generation);
            node
        };
        let parent = if self.intent.kind != Kind::Delete {
            read_record::<Ready>(&self.transaction, "ready.json")?.parent
        } else {
            None
        };
        boundary!(BeforePrepare, self.intent.generation);
        transaction_record(
            &self.held,
            &self.transaction,
            self.intent.generation,
            "prepare.json",
            &Prepare {
                intent_sha256: hash,
                entry,
                parent,
            },
        )?;
        boundary!(AfterPrepare, self.intent.generation);
        finish_prepared(&self.held, &self.transaction, &self.intent)
    }
}
fn verify_commit_snapshot(
    held: &Held,
    dir: &File,
    intent: &Intent,
    public: &File,
    current: &Value,
    actual: &Value,
) -> io::Result<()> {
    check_transaction(held, dir, intent.generation)?;
    let fresh = manifest(&held.directory)?;
    if !same_input(current, &fresh) || !same_parent(backups(&fresh), public)? {
        return Err(invalid());
    }
    let (_, destination) = if intent.kind == Kind::Delete {
        evidence::pinned_regular_manifest(dir, "archive")?
    } else {
        evidence::pinned_regular_manifest(public, &intent.id)?
    };
    if &destination != actual {
        return Err(invalid());
    }
    check_transaction(held, dir, intent.generation)
}
fn finish_prepared(held: &Held, dir: &File, intent: &Intent) -> io::Result<()> {
    check_transaction(held, dir, intent.generation)?;
    #[cfg(test)]
    let _finite_io_context = super::finite_io_tests::ContextGuard::backup(
        &held.directory,
        dir,
        intent.generation,
        super::finite_io_tests::Record::Other,
        super::finite_io_tests::Mode::Mutation,
    )?;
    let p: Prepare = read_record(dir, "prepare.json")?;
    if p.intent_sha256 != digest(intent)?
        || intent.before["root"] != manifest(&held.directory)?["root"]
    {
        return Err(invalid());
    }
    ledger::verify_backup_lifecycle_origin(&held.directory, intent.publication.as_ref())?;
    let mut public = public_dir(&held.directory, false)?;
    if let Some(parent) = &p.parent {
        let staged = exists(dir, "public-parent")?;
        if staged == public.is_some() {
            return Err(invalid());
        }
        if staged {
            let (_, actual) = evidence::pinned_empty_directory_manifest(dir, "public-parent")?;
            if &actual != parent {
                return Err(invalid());
            }
            boundary!(BeforeDirectoryRename, intent.generation);
            check_transaction(held, dir, intent.generation)?;
            proletarius_platform_fs::rename_directory_noreplace(
                dir,
                "public-parent",
                &held.directory,
                "module-backups",
            )
            .map_err(|_| invalid())?;
            boundary!(AfterDirectoryRename, intent.generation);
            dir.sync_all()?;
            held.directory.sync_all()?;
            public = public_dir(&held.directory, false)?;
        }
        if !same_parent(parent, public.as_ref().ok_or_else(invalid)?)? {
            return Err(invalid());
        }
    }
    let present = public
        .as_ref()
        .map(|p| exists(p, &intent.id))
        .transpose()?
        .unwrap_or(false);
    let private_name = if intent.kind == Kind::Delete {
        "archive"
    } else {
        "stage"
    };
    let private = exists(dir, private_name)?;
    match intent.kind {
        Kind::Delete => {
            if present == private {
                return Err(invalid());
            }
            if present {
                let public = public.as_ref().ok_or_else(invalid)?;
                let (_, actual) = evidence::pinned_regular_manifest(public, &intent.id)?;
                if actual != p.entry
                    || backups(&manifest(&held.directory)?) != backups(&intent.before)
                {
                    return Err(invalid());
                }
                boundary!(BeforeRename, intent.generation);
                check_transaction(held, dir, intent.generation)?;
                proletarius_platform_fs::rename_regular_noreplace(
                    public, &intent.id, dir, "archive",
                )
                .map_err(|_| invalid())?;
                boundary!(AfterRename, intent.generation);
            }
        }
        _ => {
            if present == private {
                return Err(invalid());
            }
            if private {
                let (_, actual) = evidence::pinned_regular_manifest(dir, "stage")?;
                let current = manifest(&held.directory)?;
                let before = backups(&intent.before);
                let unchanged = if before.is_null() {
                    children(backups(&current))?.is_empty()
                        && p.parent.as_ref().is_some_and(|parent| {
                            public
                                .as_ref()
                                .is_some_and(|file| same_parent(parent, file).unwrap_or(false))
                        })
                } else {
                    backups(&current) == before
                };
                if actual != p.entry || !unchanged {
                    return Err(invalid());
                }
                let public = public.as_ref().ok_or_else(invalid)?;
                boundary!(BeforeRename, intent.generation);
                check_transaction(held, dir, intent.generation)?;
                proletarius_platform_fs::rename_regular_noreplace(dir, "stage", public, &intent.id)
                    .map_err(|_| invalid())?;
                boundary!(AfterRename, intent.generation);
            }
        }
    }
    check_transaction(held, dir, intent.generation)?;
    let public = public_dir(&held.directory, false)?.ok_or_else(invalid)?;
    let (_, actual) = if intent.kind == Kind::Delete {
        evidence::pinned_regular_manifest(dir, "archive")?
    } else {
        evidence::pinned_regular_manifest(&public, &intent.id)?
    };
    if !same_file(&actual, &p.entry) {
        return Err(invalid());
    }
    let current = manifest(&held.directory)?;
    if !same_parent(backups(&current), &public)? {
        return Err(invalid());
    }
    if let Some(parent) = &p.parent {
        if !same_parent(parent, &public)? {
            return Err(invalid());
        }
    }
    transition(
        backups(&intent.before),
        backups(&current),
        &intent.id,
        intent.kind,
        &p.entry,
    )?;
    boundary!(BeforeSourceSync, intent.generation);
    #[cfg(test)]
    super::finite_io_tests::sync_all(
        super::finite_io_tests::SyncPoint::BackupPreparedSource,
        if intent.kind == Kind::Delete {
            &public
        } else {
            dir
        },
    )?;
    #[cfg(not(test))]
    if intent.kind == Kind::Delete {
        public.sync_all()?;
    } else {
        dir.sync_all()?;
    }
    boundary!(AfterSourceSync, intent.generation);
    boundary!(BeforeDestinationSync, intent.generation);
    if intent.kind == Kind::Delete {
        dir.sync_all()?;
    } else {
        public.sync_all()?;
    }
    boundary!(AfterDestinationSync, intent.generation);
    held.namespace.sync_all()?;
    boundary!(BeforeRootSync, intent.generation);
    held.directory.sync_all()?;
    boundary!(AfterRootSync, intent.generation);
    held.check()?;
    boundary!(BeforeCommit, intent.generation);
    verify_commit_snapshot(held, dir, intent, &public, &current, &actual)?;
    transaction_record(
        held,
        dir,
        intent.generation,
        "commit.json",
        &Commit {
            intent_sha256: digest(intent)?,
            entry: actual.clone(),
            after: backups(&current).clone(),
        },
    )?;
    boundary!(AfterCommit, intent.generation);
    held.directory.sync_all()?;
    verify_commit_snapshot(held, dir, intent, &public, &current, &actual)
}
/// Called with the real lifetime root before Gate inspection. It performs no
/// native work; only an exact original one-sided prepared layout can complete.
pub(crate) fn resume(root: &Path, proof: &RootProof) -> io::Result<()> {
    let Some(held) = Held::acquire(root, proof, false)? else {
        return Ok(());
    };
    let result = (|| {
        let v = scan(&held.directory)?.ok_or_else(invalid)?;
        if let Some(t) = v.transactions.last() {
            if t.commit.is_none() && t.abort.is_none() {
                ledger::verify_backup_lifecycle_origin(
                    &held.directory,
                    t.intent.publication.as_ref(),
                )?;
                if t.prepare.is_some() {
                    finish_prepared(&held, &t.directory, &t.intent)?;
                } else if let Some(ready) = &t.ready {
                    let (_, actual) = evidence::pinned_regular_manifest(&t.directory, "stage")?;
                    if actual != ready.entry
                        || !same_input(&t.intent.before, &manifest(&held.directory)?)
                    {
                        return Err(invalid());
                    }
                    if let Some(parent) = &ready.parent {
                        if evidence::pinned_empty_directory_manifest(&t.directory, "public-parent")?
                            .1
                            != *parent
                        {
                            return Err(invalid());
                        }
                    }
                    transaction_record(
                        &held,
                        &t.directory,
                        t.intent.generation,
                        "prepare.json",
                        &Prepare {
                            intent_sha256: digest(&t.intent)?,
                            entry: ready.entry.clone(),
                            parent: ready.parent.clone(),
                        },
                    )?;
                    finish_prepared(&held, &t.directory, &t.intent)?;
                } else {
                    // An unrecorded allocated stage is deliberately not
                    // adopted from matching bytes: its inode is unproven.
                    if t.ready.is_some()
                        || exists(&t.directory, "stage")?
                        || exists(&t.directory, "archive")?
                        || exists(&t.directory, "public-parent")?
                        || !same_input(&t.intent.before, &manifest(&held.directory)?)
                    {
                        return Err(invalid());
                    }
                    transaction_record(
                        &held,
                        &t.directory,
                        t.intent.generation,
                        "abort.json",
                        &Abort {
                            intent_sha256: digest(&t.intent)?,
                        },
                    )?;
                }
            }
        }
        if !complete(&held.directory)? {
            return Err(invalid());
        }
        Ok(())
    })();
    let release = held.release();
    match (result, release) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e),
        (_, Err(e)) => Err(e),
    }
}
