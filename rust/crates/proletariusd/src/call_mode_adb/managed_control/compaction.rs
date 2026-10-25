//! Bounded local retirement of exact closed control pairs. The private rolling
//! archive retains their exact key records and receipts, not a permanent audit log. It grants
//! no physical recovery, operation, or historical-clean authority.
use super::*;
use nix::fcntl::renameat;
use nix::sys::stat::FileStat;
use nix::unistd::{UnlinkatFlags, unlinkat};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io;
use zeroize::Zeroize;

const ARCHIVE: &str = ".closed-pairs-v1.json";
const STAGED: &str = ".closed-pairs-v1.next";
const DOMAIN: &str = "switch.voice-control.closed-pairs.archive.v1";
const BATCH: usize = 16;
const TRIGGER: usize = BATCH * 2;
const MAX_ENVELOPE: usize = 768 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024;
const MAX_NAMES: usize = 256;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "voice control evidence unavailable",
    )
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    dev: u64,
    ino: u64,
}
impl Identity {
    fn of(file: &File) -> io::Result<Self> {
        let m = file.metadata()?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    identity: Identity,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    len: u64,
    mt: (i64, i64),
    ct: (i64, i64),
}
impl Fingerprint {
    fn of(file: &File) -> io::Result<Self> {
        let m = file.metadata()?;
        Ok(Self {
            identity: Identity {
                dev: m.dev(),
                ino: m.ino(),
            },
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            links: m.nlink(),
            len: m.len(),
            mt: (m.mtime(), m.mtime_nsec()),
            ct: (m.ctime(), m.ctime_nsec()),
        })
    }
    fn stat(m: &FileStat) -> Self {
        Self {
            identity: Identity {
                dev: m.st_dev as u64,
                ino: m.st_ino as u64,
            },
            mode: m.st_mode as u32,
            uid: m.st_uid,
            gid: m.st_gid,
            links: m.st_nlink as u64,
            len: m.st_size as u64,
            mt: (m.st_mtime as i64, m.st_mtime_nsec as i64),
            ct: (m.st_ctime as i64, m.st_ctime_nsec as i64),
        }
    }
    fn regular(self) -> bool {
        self.mode & nix::libc::S_IFMT as u32 == nix::libc::S_IFREG as u32
            && self.mode & 0o7777 == 0o600
            && self.uid == nix::unistd::geteuid().as_raw()
            && self.links == 1
    }
}

fn named(dir: &File, name: &OsStr) -> io::Result<Option<Fingerprint>> {
    match fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(m) => Ok(Some(Fingerprint::stat(&m))),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn list(dir: &File) -> io::Result<BTreeSet<OsString>> {
    let names = proletarius_platform_fs::directory_names(dir, MAX_NAMES)?;
    let count = names.len();
    let set: BTreeSet<_> = names.into_iter().collect();
    if count != set.len() {
        return Err(invalid());
    }
    Ok(set)
}
fn read_exact(file: &File, stamp: Fingerprint) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(vec![0; stamp.len as usize + 1]);
    let mut off = 0;
    while off < bytes.len() {
        match file.read_at(&mut bytes[off..], off as u64) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    if off != stamp.len as usize || Fingerprint::of(file)? != stamp {
        return Err(invalid());
    }
    bytes.truncate(off);
    Ok(bytes)
}

struct Pinned {
    file: File,
    stamp: Fingerprint,
    bytes: Zeroizing<Vec<u8>>,
}
impl Pinned {
    fn capture(dir: &File, name: &OsStr, max: usize) -> io::Result<Self> {
        let stamp = named(dir, name)?.ok_or_else(invalid)?;
        if !stamp.regular() || stamp.len == 0 || stamp.len > max as u64 {
            return Err(invalid());
        }
        let file = File::from(openat(
            dir,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )?);
        if Fingerprint::of(&file)? != stamp || named(dir, name)? != Some(stamp) {
            return Err(invalid());
        }
        let bytes = read_exact(&file, stamp)?;
        let value = Self { file, stamp, bytes };
        value.validate(dir, name)?;
        Ok(value)
    }
    fn validate(&self, dir: &File, name: &OsStr) -> io::Result<()> {
        if Fingerprint::of(&self.file)? != self.stamp
            || named(dir, name)? != Some(self.stamp)
            || read_exact(&self.file, self.stamp)?.as_slice() != self.bytes.as_slice()
            || named(dir, name)? != Some(self.stamp)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Reused by startup's read-only inspector. Only this full FD-backed form may
/// establish the local closed-control exception; parsing a byte map cannot
/// establish a root/store namespace or the identity of a residual original.
pub(crate) struct ControlStoreEvidence {
    directory: File,
    stamp: Fingerprint,
    root: Identity,
    names: BTreeSet<OsString>,
    files: BTreeMap<String, Pinned>,
}
impl ControlStoreEvidence {
    pub(super) fn sessions(&self) -> io::Result<Vec<String>> {
        let mut sessions = BTreeSet::new();
        for name in self.files.keys() {
            if name == ARCHIVE || name == STAGED || name.ends_with(".complete.json") {
                continue;
            }
            let session = name
                .strip_suffix(".json")
                .filter(|s| lower_hex(s, 16))
                .ok_or_else(invalid)?;
            sessions.insert(session.to_owned());
        }
        let (archive, staged) = self.envelopes()?;
        for envelope in archive.iter().chain(staged.iter()) {
            for pair in &envelope.pairs {
                sessions.insert(pair.session.clone());
            }
        }
        if sessions.len() > MAX_NAMES {
            return Err(invalid());
        }
        Ok(sessions.into_iter().collect())
    }
    pub(super) fn verify_normal_origins(&self, origins: &[(String, String)]) -> io::Result<()> {
        self.verify_closed()?;
        self.verify_origin_records(origins)
    }
    pub(super) fn verify_origin_records(&self, origins: &[(String, String)]) -> io::Result<()> {
        let mut allowed = BTreeMap::new();
        for (session, hash) in origins {
            if !lower_hex(session, 16)
                || !lower_hex(hash, 32)
                || allowed.insert(session.as_str(), hash.as_str()).is_some()
            {
                return Err(invalid());
            }
        }
        let verify = |session: &str, bytes: &[u8]| -> io::Result<()> {
            let hash = hex(&Sha256::digest(bytes));
            if allowed.get(session).copied() != Some(hash.as_str()) {
                return Err(invalid());
            }
            Ok(())
        };
        for (name, file) in &self.files {
            if name == ARCHIVE || name == STAGED || name.ends_with(".complete.json") {
                continue;
            }
            let session = name
                .strip_suffix(".json")
                .filter(|s| lower_hex(s, 16))
                .ok_or_else(invalid)?;
            verify(session, &file.bytes)?;
        }
        let (archive, staged) = self.envelopes()?;
        for envelope in archive.iter().chain(staged.iter()) {
            for pair in &envelope.pairs {
                verify(&pair.session, &decode(&pair.record, MAX_RECORD)?)?;
            }
        }
        Ok(())
    }
    pub(super) fn capture_with_pending(
        root: &File,
        binding: &DurableControlBinding,
    ) -> io::Result<Self> {
        let mut value = Self::capture_inner(root, false)?;
        let name = format!("{}.json", binding.session);
        let pending = value.files.remove(&name).ok_or_else(invalid)?;
        if hex(&Sha256::digest(&pending.bytes[..])) != binding.record_sha256
            || !value.names.remove(OsStr::new(&name))
        {
            return Err(invalid());
        }
        let result = if value.names.is_empty() {
            Ok(())
        } else {
            value.verify_closed()
        };
        value.names.insert(name.clone().into());
        value.files.insert(name, pending);
        result?;
        value.validate(root)?;
        Ok(value)
    }
    pub(crate) fn capture(root: &File) -> io::Result<Self> {
        let value = Self::capture_inner(root, false)?;
        value.verify_closed()?;
        value.validate(root)?;
        Ok(value)
    }
    fn capture_inner(root: &File, allow_opaque: bool) -> io::Result<Self> {
        let stamp = named(root, OsStr::new(STORE))?.ok_or_else(invalid)?;
        let directory = File::from(openat(
            root,
            STORE,
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        super::directory(&directory).map_err(|_| invalid())?;
        if Fingerprint::of(&directory)? != stamp {
            return Err(invalid());
        }
        let names = list(&directory)?;
        let mut files = BTreeMap::new();
        let mut total = 0usize;
        for name in &names {
            let Some(text) = name.to_str() else {
                if allow_opaque {
                    continue;
                } else {
                    return Err(invalid());
                }
            };
            let envelope = text == ARCHIVE || text == STAGED;
            let limit = if envelope { MAX_ENVELOPE } else { 16_384 };
            let file = match Pinned::capture(&directory, name, limit) {
                Ok(file) => file,
                Err(_) if allow_opaque && !envelope => continue,
                Err(error) => return Err(error),
            };
            let next_total = total.checked_add(file.bytes.len()).ok_or_else(invalid)?;
            if next_total > MAX_TOTAL {
                if allow_opaque && !envelope {
                    continue;
                }
                return Err(invalid());
            }
            total = next_total;
            files.insert(text.to_owned(), file);
        }
        let value = Self {
            directory,
            stamp,
            root: Identity::of(root)?,
            names,
            files,
        };
        value.validate(root)?;
        Ok(value)
    }
    pub(crate) fn validate(&self, root: &File) -> io::Result<()> {
        self.namespace(root)?;
        if Fingerprint::of(&self.directory)? != self.stamp || list(&self.directory)? != self.names {
            return Err(invalid());
        }
        for (name, file) in &self.files {
            file.validate(&self.directory, OsStr::new(name))?;
        }
        self.namespace(root)?;
        if Fingerprint::of(&self.directory)? != self.stamp || list(&self.directory)? != self.names {
            return Err(invalid());
        }
        Ok(())
    }
    fn namespace(&self, root: &File) -> io::Result<()> {
        super::directory(&self.directory).map_err(|_| invalid())?;
        if Identity::of(root)? != self.root
            || Identity::of(&self.directory)? != self.stamp.identity
            || named(root, OsStr::new(STORE))?.map(|m| m.identity) != Some(self.stamp.identity)
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn envelopes(&self) -> io::Result<(Option<Envelope>, Option<Envelope>)> {
        let archive = self
            .files
            .get(ARCHIVE)
            .map(|f| Envelope::parse(&f.bytes))
            .transpose()?;
        let staged = self
            .files
            .get(STAGED)
            .map(|f| Envelope::parse(&f.bytes))
            .transpose()?;
        for envelope in archive.iter().chain(staged.iter()) {
            if envelope.root != self.root || envelope.store != self.stamp.identity {
                return Err(invalid());
            }
            self.residuals(envelope, false)?;
        }
        if let Some(next) = &staged {
            let previous = self
                .files
                .get(ARCHIVE)
                .map(|f| hex(&Sha256::digest(&f.bytes[..])));
            if next.previous_archive_sha256 != previous {
                return Err(invalid());
            }
            if let Some(old) = &archive {
                self.require_absent(old)?;
            }
            self.residuals(next, true)?;
        }
        Ok((archive, staged))
    }
    fn residuals(&self, envelope: &Envelope, all: bool) -> io::Result<()> {
        for pair in &envelope.pairs {
            for (name, stamp, bytes) in pair.parts()? {
                match self.files.get(&name) {
                    Some(file)
                        if file.stamp == stamp && file.bytes.as_slice() == bytes.as_slice() => {}
                    None if !all && !self.names.contains(OsStr::new(&name)) => {}
                    _ => return Err(invalid()),
                }
            }
        }
        Ok(())
    }
    fn require_absent(&self, envelope: &Envelope) -> io::Result<()> {
        if envelope.pairs.iter().any(|pair| {
            self.names.contains(OsStr::new(&pair.record_name()))
                || self.names.contains(OsStr::new(&pair.completion_name()))
        }) {
            return Err(invalid());
        }
        Ok(())
    }
    fn verify_closed(&self) -> io::Result<()> {
        if self.names.is_empty() || self.names.len() != self.files.len() {
            return Err(invalid());
        }
        let (archive, staged) = self.envelopes()?;
        let mut covered = BTreeSet::new();
        for (name, envelope) in [(ARCHIVE, archive.as_ref()), (STAGED, staged.as_ref())] {
            if let Some(envelope) = envelope {
                covered.insert(name.to_owned());
                for pair in &envelope.pairs {
                    for (name, _, _) in pair.parts()? {
                        if self.files.contains_key(&name) {
                            covered.insert(name);
                        }
                    }
                }
            }
        }
        for (name, record) in &self.files {
            if covered.contains(name) || name.ends_with(".complete.json") {
                continue;
            }
            let session = name
                .strip_suffix(".json")
                .filter(|s| lower_hex(s, 16))
                .ok_or_else(invalid)?;
            let cname = format!("{session}.complete.json");
            let complete = self.files.get(&cname).ok_or_else(invalid)?;
            verify_closed_pair(session, &record.bytes, &complete.bytes).map_err(|_| invalid())?;
            covered.insert(name.clone());
            covered.insert(cname);
        }
        if covered.len() != self.files.len() {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedPair {
    session: String,
    record: String,
    completion: String,
    record_stamp: Fingerprint,
    completion_stamp: Fingerprint,
}
impl Drop for ArchivedPair {
    fn drop(&mut self) {
        self.record.zeroize();
        self.completion.zeroize();
    }
}
impl ArchivedPair {
    fn record_name(&self) -> String {
        format!("{}.json", self.session)
    }
    fn completion_name(&self) -> String {
        format!("{}.complete.json", self.session)
    }
    fn parts(&self) -> io::Result<[(String, Fingerprint, Zeroizing<Vec<u8>>); 2]> {
        Ok([
            (
                self.record_name(),
                self.record_stamp,
                decode(&self.record, MAX_RECORD)?,
            ),
            (
                self.completion_name(),
                self.completion_stamp,
                decode(&self.completion, 16_384)?,
            ),
        ])
    }
}
fn decode(text: &str, max: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    if text.is_empty()
        || text.len() > max * 2
        || text.len() % 2 != 0
        || !lower_hex(text, text.len() / 2)
    {
        return Err(invalid());
    }
    let nibble = |b: u8| {
        if b.is_ascii_digit() {
            b - b'0'
        } else {
            b - b'a' + 10
        }
    };
    Ok(Zeroizing::new(
        text.as_bytes()
            .chunks_exact(2)
            .map(|p| (nibble(p[0]) << 4) | nibble(p[1]))
            .collect(),
    ))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    domain: String,
    schema_version: u8,
    root: Identity,
    store: Identity,
    previous_archive_sha256: Option<String>,
    pairs: Vec<ArchivedPair>,
}
impl Envelope {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_ENVELOPE {
            return Err(invalid());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if value.domain != DOMAIN
            || value.schema_version != 1
            || value.pairs.is_empty()
            || value.pairs.len() > BATCH
            || value
                .previous_archive_sha256
                .as_ref()
                .is_some_and(|s| !lower_hex(s, 32))
        {
            return Err(invalid());
        }
        let mut sessions = BTreeSet::new();
        for pair in &value.pairs {
            if !lower_hex(&pair.session, 16)
                || !sessions.insert(&pair.session)
                || !pair.record_stamp.regular()
                || !pair.completion_stamp.regular()
            {
                return Err(invalid());
            }
            let [(_, rs, record), (_, cs, complete)] = pair.parts()?;
            if rs.len as usize != record.len() || cs.len as usize != complete.len() {
                return Err(invalid());
            }
            verify_closed_pair(&pair.session, &record, &complete).map_err(|_| invalid())?;
        }
        Ok(value)
    }
}

// Pure format checking remains useful to callers/tests, but it is deliberately
// insufficient for namespace or inode authority. Startup uses the FD snapshot.
#[cfg(test)]
pub(super) fn completed_payloads(files: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    let result = || -> io::Result<()> {
        if files.is_empty()
            || files.len() > MAX_NAMES
            || files.values().map(Vec::len).sum::<usize>() > MAX_TOTAL
        {
            return Err(invalid());
        }
        if !files.contains_key(ARCHIVE) && !files.contains_key(STAGED) {
            return completed_ordinary_store_files(files).map_err(|_| invalid());
        }
        let archive = files.get(ARCHIVE).map(|b| Envelope::parse(b)).transpose()?;
        let staged = files.get(STAGED).map(|b| Envelope::parse(b)).transpose()?;
        if let Some(next) = &staged {
            if next.previous_archive_sha256 != files.get(ARCHIVE).map(|b| hex(&Sha256::digest(b))) {
                return Err(invalid());
            }
            if let Some(old) = &archive {
                if old.root != next.root
                    || old.store != next.store
                    || old.pairs.iter().any(|p| {
                        files.contains_key(&p.record_name())
                            || files.contains_key(&p.completion_name())
                    })
                {
                    return Err(invalid());
                }
            }
        }
        let mut covered = BTreeSet::new();
        for (name, envelope, all) in [
            (ARCHIVE, archive.as_ref(), false),
            (STAGED, staged.as_ref(), true),
        ] {
            if let Some(envelope) = envelope {
                covered.insert(name.to_owned());
                for pair in &envelope.pairs {
                    for (name, _, bytes) in pair.parts()? {
                        match files.get(&name) {
                            Some(actual) if actual.as_slice() == bytes.as_slice() => {
                                covered.insert(name);
                            }
                            None if !all => {}
                            _ => return Err(invalid()),
                        }
                    }
                }
            }
        }
        for (name, record) in files {
            if covered.contains(name) || name.ends_with(".complete.json") {
                continue;
            }
            let session = name
                .strip_suffix(".json")
                .filter(|s| lower_hex(s, 16))
                .ok_or_else(invalid)?;
            let cname = format!("{session}.complete.json");
            verify_closed_pair(session, record, files.get(&cname).ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            covered.insert(name.clone());
            covered.insert(cname);
        }
        if covered.len() != files.len() {
            return Err(invalid());
        }
        Ok(())
    };
    result().map_err(|_| "managed voice control completion unavailable".into())
}

// File::try_lock uses a kernel flock attached to this independently opened
// description. It conflicts with the retained owner's shared record pin.
// Single-root daemon exclusion is still mandatory for pre-pin old versions.
struct ExclusivePins<'a>(Vec<&'a File>);
impl Drop for ExclusivePins<'_> {
    fn drop(&mut self) {
        for file in &self.0 {
            if file.unlock().is_err() {
                crate::diagnostics::write_line(format_args!(
                    "voice control retirement pin release failed"
                ));
            }
        }
    }
}
fn pin<'a>(file: &'a File, pins: &mut ExclusivePins<'a>) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => {
            pins.0.push(file);
            Ok(true)
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

pub(super) fn maintain(
    root: &File,
    directory: &File,
    check: &impl Fn() -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    let names = list(directory).map_err(io_error)?;
    if names.len() < TRIGGER
        && !names.contains(OsStr::new(ARCHIVE))
        && !names.contains(OsStr::new(STAGED))
    {
        return Ok(());
    }
    let snapshot = ControlStoreEvidence::capture_inner(root, true).map_err(io_error)?;
    if Identity::of(directory).map_err(io_error)? != snapshot.stamp.identity {
        return Err(failure());
    }
    let (archive, staged) = snapshot.envelopes().map_err(io_error)?;
    if let Some(next) = staged {
        let mut pins = ExclusivePins(Vec::new());
        for pair in &next.pairs {
            let record = snapshot
                .files
                .get(&pair.record_name())
                .ok_or_else(failure)?;
            if !pin(&record.file, &mut pins).map_err(io_error)? {
                return Err(failure());
            }
        }
        commit(&snapshot, root, check)?;
        retire(&snapshot, &next, root, check)?;
        return Ok(());
    }
    if let Some(old) = &archive
        && snapshot.require_absent(old).is_err()
    {
        let mut pins = ExclusivePins(Vec::new());
        for pair in &old.pairs {
            if let Some(record) = snapshot.files.get(&pair.record_name()) {
                if !pin(&record.file, &mut pins).map_err(io_error)? {
                    return Err(failure());
                }
            }
        }
        // Sync an observed commit again before resuming interrupted removal.
        snapshot
            .files
            .get(ARCHIVE)
            .ok_or_else(failure)?
            .file
            .sync_all()
            .map_err(io_error)?;
        snapshot.directory.sync_all().map_err(io_error)?;
        root.sync_all().map_err(io_error)?;
        retire(&snapshot, old, root, check)?;
    }
    // Our own removals changed the directory. Capture the actual resulting
    // namespace again instead of treating the pre-removal snapshot as valid.
    let snapshot = ControlStoreEvidence::capture_inner(root, true).map_err(io_error)?;
    if snapshot.names.len() < TRIGGER {
        return Ok(());
    }
    let mut pins = ExclusivePins(Vec::new());
    let mut pairs = Vec::new();
    for (name, record) in &snapshot.files {
        if pairs.len() == BATCH {
            break;
        }
        let Some(session) = name.strip_suffix(".json").filter(|s| lower_hex(s, 16)) else {
            continue;
        };
        let Some(complete) = snapshot.files.get(&format!("{session}.complete.json")) else {
            continue;
        };
        if verify_closed_pair(session, &record.bytes, &complete.bytes).is_err() {
            continue;
        }
        if !pin(&record.file, &mut pins).map_err(io_error)? {
            continue;
        }
        pairs.push(ArchivedPair {
            session: session.to_owned(),
            record: hex(&record.bytes),
            completion: hex(&complete.bytes),
            record_stamp: record.stamp,
            completion_stamp: complete.stamp,
        });
    }
    // Pending/partial/unknown/live-pinned originals do not add an earlier
    // capacity failure; the existing hard create guard remains authoritative.
    if pairs.is_empty() {
        return Ok(());
    }
    snapshot.validate(root).map_err(io_error)?;
    let next = Envelope {
        domain: DOMAIN.into(),
        schema_version: 1,
        root: snapshot.root,
        store: snapshot.stamp.identity,
        previous_archive_sha256: snapshot
            .files
            .get(ARCHIVE)
            .map(|f| hex(&Sha256::digest(&f.bytes[..]))),
        pairs,
    };
    let payload = Zeroizing::new(serde_json::to_vec(&next).map_err(io_error)?);
    if payload.len() > MAX_ENVELOPE
        || snapshot
            .files
            .values()
            .map(|f| f.bytes.len())
            .sum::<usize>()
            + payload.len()
            > MAX_TOTAL
        || snapshot.names.len() >= MAX_NAMES
    {
        // No staging mutation has happened: insufficient inspection/staging
        // budget may defer GC; create's original hard name cap still applies.
        return Ok(());
    }
    check()?;
    snapshot.validate(root).map_err(io_error)?;
    let mut file = File::from(
        openat(
            &snapshot.directory,
            STAGED,
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(io_error)?,
    );
    // Any interrupted/short write stays visible and fail-closed. No cleanup
    // guard may silently remove an uncommitted partial file.
    file.write_all(&payload).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    snapshot.directory.sync_all().map_err(io_error)?;
    root.sync_all().map_err(io_error)?;
    let staged = ControlStoreEvidence::capture_inner(root, true).map_err(io_error)?;
    let observed = staged.files.get(STAGED).ok_or_else(failure)?;
    if observed.bytes.as_slice() != payload.as_slice()
        || Identity::of(&file).map_err(io_error)? != observed.stamp.identity
    {
        return Err(failure());
    }
    staged.envelopes().map_err(io_error)?;
    commit(&staged, root, check)?;
    retire(&staged, &next, root, check)
}

fn commit(
    snapshot: &ControlStoreEvidence,
    root: &File,
    check: &impl Fn() -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    check()?;
    snapshot.validate(root).map_err(io_error)?;
    snapshot.envelopes().map_err(io_error)?;
    let staged = snapshot.files.get(STAGED).ok_or_else(failure)?;
    staged.file.sync_all().map_err(io_error)?;
    check()?;
    snapshot.validate(root).map_err(io_error)?;
    renameat(&snapshot.directory, STAGED, &snapshot.directory, ARCHIVE).map_err(io_error)?;
    snapshot.directory.sync_all().map_err(io_error)?;
    root.sync_all().map_err(io_error)?;
    snapshot.namespace(root).map_err(io_error)?;
    let committed = Pinned::capture(&snapshot.directory, OsStr::new(ARCHIVE), MAX_ENVELOPE)
        .map_err(io_error)?;
    if committed.stamp.identity != staged.stamp.identity
        || committed.bytes.as_slice() != staged.bytes.as_slice()
        || named(&snapshot.directory, OsStr::new(STAGED))
            .map_err(io_error)?
            .is_some()
    {
        return Err(failure());
    }
    committed.file.sync_all().map_err(io_error)?;
    check()
}

fn retire(
    snapshot: &ControlStoreEvidence,
    envelope: &Envelope,
    root: &File,
    check: &impl Fn() -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    let archive = Pinned::capture(&snapshot.directory, OsStr::new(ARCHIVE), MAX_ENVELOPE)
        .map_err(io_error)?;
    let expected = Zeroizing::new(serde_json::to_vec(envelope).map_err(io_error)?);
    if archive.bytes.as_slice() != expected.as_slice() {
        return Err(failure());
    }
    for pair in &envelope.pairs {
        for (name, stamp, bytes) in pair.parts().map_err(io_error)? {
            check()?;
            snapshot.namespace(root).map_err(io_error)?;
            archive
                .validate(&snapshot.directory, OsStr::new(ARCHIVE))
                .map_err(io_error)?;
            match named(&snapshot.directory, OsStr::new(&name)).map_err(io_error)? {
                None => continue,
                Some(actual) if actual == stamp => {}
                _ => return Err(failure()),
            }
            let original = snapshot.files.get(&name).ok_or_else(failure)?;
            original
                .validate(&snapshot.directory, OsStr::new(&name))
                .map_err(io_error)?;
            if original.stamp != stamp || original.bytes.as_slice() != bytes.as_slice() {
                return Err(failure());
            }
            check()?;
            snapshot.namespace(root).map_err(io_error)?;
            original
                .validate(&snapshot.directory, OsStr::new(&name))
                .map_err(io_error)?;
            unlinkat(
                &snapshot.directory,
                name.as_str(),
                UnlinkatFlags::NoRemoveDir,
            )
            .map_err(io_error)?;
            if named(&snapshot.directory, OsStr::new(&name))
                .map_err(io_error)?
                .is_some()
            {
                return Err(failure());
            }
        }
    }
    snapshot.directory.sync_all().map_err(io_error)?;
    root.sync_all().map_err(io_error)?;
    archive
        .validate(&snapshot.directory, OsStr::new(ARCHIVE))
        .map_err(io_error)?;
    snapshot.namespace(root).map_err(io_error)?;
    check()
}

#[cfg(test)]
mod tests;
