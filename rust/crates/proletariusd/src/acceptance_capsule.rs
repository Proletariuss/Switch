//! G8 pre-entry validation only. This does not grant runtime admission or exec
//! custody: descriptors pin observed objects, not another same-UID process's
//! future writes/exec. The shipping entry point always refuses this lane (78).
#[allow(dead_code)]
#[path = "acceptance_capsule/phase_b.rs"]
mod phase_b;

use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::Mode,
    unistd::{getppid, getuid},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::fs::MetadataExt;
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
const PURPOSE: &str = "switch-no-device-software-acceptance";
const LIMIT: u64 = 16384;
const CHILD: [&str; 5] = [
    "app_pid",
    "parent_manifest_sha256",
    "parent_manifest_inode",
    "parent_manifest_path",
    "child_nonce",
];
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u64,
    role: String,
    purpose: String,
    run_id: String,
    nonce: String,
    guardian_pid: u64,
    uid: u64,
    created_unix_ms: u64,
    expires_unix_ms: u64,
    capsule_root: String,
    data_root: String,
    evidence_root: String,
    tmp_root: String,
    capsule_root_dev: u64,
    capsule_root_ino: u64,
    data_root_dev: u64,
    data_root_ino: u64,
    evidence_root_dev: u64,
    evidence_root_ino: u64,
    tmp_root_dev: u64,
    tmp_root_ino: u64,
    app_path: String,
    app_sha256: String,
    backend_path: String,
    backend_sha256: String,
    guardian_path: String,
    guardian_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_pid: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_manifest_inode: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_manifest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_nonce: Option<String>,
}
fn invalid<T>() -> Result<T> {
    Err("acceptance capsule rejected".into())
}
fn check(ok: bool) -> Result<()> {
    if ok { Ok(()) } else { invalid() }
}
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| "acceptance capsule I/O rejected".into())
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(crate) fn canonical(path: &str) -> Result<()> {
    check(
        path.starts_with('/')
            && path != "/"
            && !path.ends_with('/')
            && !path.contains('\0')
            && !path.contains("//")
            && !path.split('/').any(|s| s == "." || s == ".."),
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
    links: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Identity {
    fn of(file: &File) -> Result<Self> {
        let m = io(file.metadata())?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            uid: m.uid(),
            mode: m.mode(),
            links: m.nlink(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
    }
    fn directory_eq(&self, b: &Self) -> bool {
        (self.dev, self.ino, self.uid, self.mode) == (b.dev, b.ino, b.uid, b.mode)
    }
    fn regular(&self, private: bool, uid: u32) -> Result<()> {
        check(
            self.mode & 0o170000 == 0o100000
                && self.links == 1
                && (!private || (self.uid == uid && matches!(self.mode & 0o7777, 0o600 | 0o400))),
        )
    }
}
struct Held {
    path: String,
    file: File,
    identity: Identity,
    directory: bool,
}
impl Held {
    fn revalidate(&self) -> Result<()> {
        let actual = Identity::of(&self.file)?;
        check(if self.directory {
            self.identity.directory_eq(&actual)
        } else {
            self.identity == actual
        })?;
        let reopened = walk(&self.path, self.directory)?;
        check(self.matches(reopened.last().ok_or("empty traversal")?))
    }
    fn matches(&self, other: &Held) -> bool {
        if self.directory {
            self.identity.directory_eq(&other.identity)
        } else {
            self.identity == other.identity
        }
    }
}
/// Every component starts at / and uses O_NOFOLLOW. All ancestors stay held.
fn walk(path: &str, directory: bool) -> Result<Vec<Held>> {
    canonical(path)?;
    let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
    let root =
        open("/", flags | OFlag::O_DIRECTORY, Mode::empty()).map_err(|_| "acceptance root open")?;
    let root = File::from(root);
    let mut held = vec![Held {
        path: "/".into(),
        identity: Identity::of(&root)?,
        file: root,
        directory: true,
    }];
    let parts: Vec<_> = path[1..].split('/').collect();
    let mut current = String::new();
    for (index, part) in parts.iter().enumerate() {
        let is_directory = directory || index + 1 < parts.len();
        let file = File::from(
            openat(
                &held.last().unwrap().file,
                *part,
                flags
                    | if is_directory {
                        OFlag::O_DIRECTORY
                    } else {
                        OFlag::empty()
                    },
                Mode::empty(),
            )
            .map_err(|_| "acceptance path open")?,
        );
        current.push('/');
        current.push_str(part);
        held.push(Held {
            path: current.clone(),
            identity: Identity::of(&file)?,
            file,
            directory: is_directory,
        });
    }
    Ok(held)
}
fn revalidate_all(held: &[Held]) -> Result<()> {
    for h in held {
        // / is the fixed traversal origin and is not a canonical capsule path.
        if h.path == "/" {
            check(h.identity.directory_eq(&Identity::of(&h.file)?))?;
        } else {
            h.revalidate()?;
        }
    }
    Ok(())
}
fn read_manifest(
    path: &str,
    uid: u32,
    child: bool,
) -> Result<(Manifest, Value, Vec<Held>, Vec<u8>)> {
    let held = walk(path, false)?;
    let file = held.last().unwrap();
    file.identity.regular(true, uid)?;
    check(file.identity.size > 0 && file.identity.size <= LIMIT)?;
    let mut bytes = Vec::new();
    io((&file.file).take(LIMIT + 1).read_to_end(&mut bytes))?;
    check(bytes.len() as u64 <= LIMIT)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "acceptance schema")?;
    check(serde_json::to_vec(&value).map_err(|_| "acceptance encoding")? == bytes)?;
    let object = value.as_object().ok_or("acceptance object")?;
    check(CHILD.iter().all(|key| object.contains_key(*key) == child))?;
    let manifest: Manifest =
        serde_json::from_value(value.clone()).map_err(|_| "acceptance schema")?;
    check(object.len() == if child { 32 } else { 27 })?;
    revalidate_all(&held)?;
    Ok((manifest, value, held, bytes))
}
fn hash(path: &str, held: &mut Vec<Held>, uid: u32) -> Result<String> {
    let mut opened = walk(path, false)?;
    let file = opened.last_mut().unwrap();
    file.identity.regular(false, uid)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = io(file.file.read(&mut buffer))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    revalidate_all(&opened)?; // includes fresh / traversal after hashing every image
    held.extend(opened);
    Ok(format!("{:x}", digest.finalize()))
}
fn common(m: &Manifest, now: u64, uid: u32, max_ttl: u64) -> Result<()> {
    check(
        m.schema_version == 1
            && m.purpose == PURPOSE
            && m.uid == uid as u64
            && hex(&m.run_id, 32)
            && hex(&m.nonce, 64)
            && m.created_unix_ms > 0
            && m.created_unix_ms <= now
            && m.expires_unix_ms > now
            && m.expires_unix_ms > m.created_unix_ms
            && m.expires_unix_ms - m.created_unix_ms <= max_ttl,
    )?;
    for (path, suffix) in [
        (&m.data_root, "data"),
        (&m.evidence_root, "evidence"),
        (&m.tmp_root, "tmp"),
    ] {
        canonical(path)?;
        check(*path == format!("{}/{suffix}", m.capsule_root))?;
    }
    canonical(&m.capsule_root)
}
/// Only a validated observation. No hardware/network owner or store is exposed.
#[allow(dead_code)]
pub(crate) struct AcceptanceContext {
    held: Vec<Held>,
    pub(crate) data_root: String,
    pub(crate) run_id: String,
}
trait ProcessProof {
    fn parent(&self) -> i32;
    fn executable(&self) -> Result<PathBuf>;
    fn identity(&self, pid: i32) -> Result<(i32, PathBuf)>;
    fn environment(&self, name: &str) -> Option<String>;
}
struct Kernel;
impl ProcessProof for Kernel {
    fn parent(&self) -> i32 {
        getppid().as_raw()
    }
    fn executable(&self) -> Result<PathBuf> {
        io(std::env::current_exe())
    }
    fn identity(&self, pid: i32) -> Result<(i32, PathBuf)> {
        io(proletarius_platform_fs::process_identity(pid))
    }
    fn environment(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}
pub(crate) fn load(path: &str) -> Result<AcceptanceContext> {
    validate(path, &Kernel, now_ms()?, || Ok(()))
}
fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock")?
        .as_millis()
        .try_into()
        .map_err(|_| "clock")?)
}
fn validate(
    path: &str,
    proof: &impl ProcessProof,
    now: u64,
    after_hash: impl FnOnce() -> Result<()>,
) -> Result<AcceptanceContext> {
    let uid = getuid().as_raw();
    let (m, child_value, mut held, _) = read_manifest(path, uid, true)?;
    common(&m, now, uid, 30000)?;
    check(
        m.role == "daemon"
            && m.app_pid == Some(proof.parent() as u64)
            && proof.parent() > 1
            && m.child_nonce.as_deref().is_some_and(|s| hex(s, 64)),
    )?;
    check(
        path == format!(
            "{}/daemon-{}.json",
            m.data_root,
            m.child_nonce.as_ref().unwrap()
        ),
    )?;
    let parent_path = m.parent_manifest_path.as_deref().ok_or("parent path")?;
    // The delegation grammar has one parent filename. Hash/inode binding does
    // not authorize a renamed sibling with otherwise identical contents.
    check(parent_path == format!("{}/app.json", m.capsule_root))?;
    let (parent, pvalue, mut parent_held, parent_bytes) = read_manifest(parent_path, uid, false)?;
    common(&parent, now, uid, 120000)?;
    check(
        parent.role == "app"
            && m.expires_unix_ms <= parent.expires_unix_ms
            && m.created_unix_ms >= parent.created_unix_ms
            && m.parent_manifest_inode == Some(parent_held.last().unwrap().identity.ino)
            && m.parent_manifest_sha256
                .as_deref()
                .is_some_and(|s| hex(s, 64) && s == format!("{:x}", Sha256::digest(&parent_bytes))),
    )?;
    let mut inherited = child_value.as_object().unwrap().clone();
    for key in CHILD {
        inherited.remove(key);
    }
    for key in ["role", "created_unix_ms", "expires_unix_ms"] {
        inherited.remove(key);
    }
    let mut p_inherited = pvalue.as_object().unwrap().clone();
    for key in ["role", "created_unix_ms", "expires_unix_ms"] {
        p_inherited.remove(key);
    }
    check(inherited == p_inherited)?;
    held.append(&mut parent_held);
    for (root, dev, ino) in [
        (&m.capsule_root, m.capsule_root_dev, m.capsule_root_ino),
        (&m.data_root, m.data_root_dev, m.data_root_ino),
        (&m.evidence_root, m.evidence_root_dev, m.evidence_root_ino),
        (&m.tmp_root, m.tmp_root_dev, m.tmp_root_ino),
    ] {
        let mut directory = walk(root, true)?;
        let id = &directory.last().unwrap().identity;
        check(id.uid == uid && id.mode & 0o7777 == 0o700 && id.dev == dev && id.ino == ino)?;
        held.append(&mut directory);
    }
    authority(&m, proof)?;
    for (path, expected) in [
        (&m.app_path, &m.app_sha256),
        (&m.backend_path, &m.backend_sha256),
        (&m.guardian_path, &m.guardian_sha256),
    ] {
        check(hex(expected, 64) && hash(path, &mut held, uid)? == *expected)?;
    }
    for (name, expected) in [
        ("HOME", &m.data_root),
        ("CFFIXED_USER_HOME", &m.data_root),
        ("TMPDIR", &m.tmp_root),
    ] {
        check(proof.environment(name).as_deref() == Some(expected))?;
    }
    after_hash()?;
    revalidate_all(&held)?;
    authority(&m, proof)?;
    // Recheck wall time after expensive reads; tests use a fixed logical clock.
    #[cfg(not(test))]
    check(m.expires_unix_ms > now_ms()?)?;
    let root = held
        .iter()
        .find(|h| h.path == m.data_root && h.directory)
        .ok_or("claim root")?;
    let name = format!(".consumed-child-{}", m.child_nonce.as_ref().unwrap());
    let mut claim = File::from(
        openat(
            &root.file,
            name.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|_| "acceptance child nonce reused")?,
    );
    Identity::of(&claim)?.regular(true, uid)?;
    io(claim.write_all(b"consumed\n"))?;
    io(claim.sync_all())?;
    io(root.file.sync_all())?;
    // Never continue if the pathname moved during the atomic claim.
    revalidate_all(&held)?;
    authority(&m, proof)?;
    #[cfg(not(test))]
    check(m.expires_unix_ms > now_ms()?)?;
    Ok(AcceptanceContext {
        held,
        data_root: m.data_root,
        run_id: m.run_id,
    })
}
fn authority(m: &Manifest, proof: &impl ProcessProof) -> Result<()> {
    let app_pid = proof.parent();
    check(app_pid > 1 && m.app_pid == Some(app_pid as u64))?;
    let (guardian_pid, app_path) = proof.identity(app_pid)?;
    // Swift delegation is exactly guardian -> App -> daemon, not a PID claimed
    // by JSON or an unrelated process with the same executable name.
    check(
        guardian_pid > 1
            && m.guardian_pid == guardian_pid as u64
            && app_path == PathBuf::from(&m.app_path),
    )?;
    let (_, guardian_path) = proof.identity(guardian_pid)?;
    check(
        guardian_path == PathBuf::from(&m.guardian_path)
            && proof.executable()? == PathBuf::from(&m.backend_path),
    )
}
/// Shared by shipping main/run and the offline sentinel. A validated context
/// still cannot enter production composition in this slice.
pub(crate) fn pre_entry(arguments: &[std::ffi::OsString]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    if !arguments.iter().any(|a| {
        a.as_bytes().starts_with(b"--switch-acceptance")
            || a.as_bytes().starts_with(b"-switch-acceptance")
    }) {
        return Ok(());
    }
    let parsed = crate::daemon_args::parse(arguments.iter().cloned())?;
    if let Some(path) = parsed.acceptance_capsule {
        let _context = load(&path)?;
        return Err("acceptance daemon composition is NO-GO".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "acceptance_capsule/tests.rs"]
mod tests;
