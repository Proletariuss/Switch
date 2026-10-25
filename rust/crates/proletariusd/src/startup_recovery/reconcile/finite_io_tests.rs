//! Private test-only bounded I/O programs. No hook can manufacture authority,
//! a successful sync, physical readback, receipt, or completion token.
use crate::daemon_lifetime::RootProof;
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::Mode as OpenMode;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum Record {
    Stage,
    Ready,
    Commit,
    Requested,
    PublishedManifest,
    CompletionReceipt,
    JournalRetirement,
    Other,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum Mode {
    Mutation,
    Resync,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum SyncPoint {
    File,
    RecordParent,
    BackupPreparedSource,
    RecoveryArchive,
    AppendRoot,
    ResyncRecord,
    ResyncData,
    ResyncContainer,
    ResyncPublic,
    ResyncNamespace,
    ResyncOuter,
    ResyncRoot,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum CaseId {
    A01,
    A02,
    A03,
    A04,
    A05,
    A06,
    A07,
    A08,
    A09,
    A10,
    B01,
    B02,
    B03,
}
impl CaseId {
    pub(super) fn parse(s: &str) -> io::Result<Self> {
        Ok(match s {
            "A01" => Self::A01,
            "A02" => Self::A02,
            "A03" => Self::A03,
            "A04" => Self::A04,
            "A05" => Self::A05,
            "A06" => Self::A06,
            "A07" => Self::A07,
            "A08" => Self::A08,
            "A09" => Self::A09,
            "A10" => Self::A10,
            "B01" => Self::B01,
            "B02" => Self::B02,
            "B03" => Self::B03,
            _ => return Err(invalid()),
        })
    }
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::A01 => "A01",
            Self::A02 => "A02",
            Self::A03 => "A03",
            Self::A04 => "A04",
            Self::A05 => "A05",
            Self::A06 => "A06",
            Self::A07 => "A07",
            Self::A08 => "A08",
            Self::A09 => "A09",
            Self::A10 => "A10",
            Self::B01 => "B01",
            Self::B02 => "B02",
            Self::B03 => "B03",
        }
    }
    fn backup(self) -> bool {
        matches!(
            self,
            Self::A01 | Self::A02 | Self::A03 | Self::A04 | Self::A05 | Self::A06 | Self::B01
        )
    }
    fn selection(self) -> (Record, Option<SyncPoint>, Action) {
        match self {
            Self::A01 => (Record::Stage, None, Action::ShortOnce),
            Self::A02 => (Record::Stage, None, Action::PrefixThenEio),
            Self::A03 => (Record::Ready, None, Action::PrefixThenEio),
            Self::A04 => (Record::Commit, None, Action::PrefixThenEio),
            Self::A05 => (Record::Stage, Some(SyncPoint::File), Action::SyncEio),
            Self::A06 => (
                Record::Other,
                Some(SyncPoint::BackupPreparedSource),
                Action::SyncEio,
            ),
            Self::A07 => (Record::Requested, None, Action::PrefixThenEio),
            Self::A08 => (Record::PublishedManifest, None, Action::PrefixThenEio),
            Self::A09 => (Record::CompletionReceipt, None, Action::PrefixThenEio),
            Self::A10 => (
                Record::JournalRetirement,
                Some(SyncPoint::RecoveryArchive),
                Action::SyncEio,
            ),
            Self::B01 => (Record::Commit, Some(SyncPoint::File), Action::SyncEio),
            Self::B02 => (
                Record::CompletionReceipt,
                Some(SyncPoint::File),
                Action::SyncEio,
            ),
            Self::B03 => (
                Record::CompletionReceipt,
                Some(SyncPoint::AppendRoot),
                Action::SyncEio,
            ),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum Action {
    ShortOnce,
    PrefixThenEio,
    SyncEio,
}
fn invalid() -> io::Error {
    io::Error::other("SWITCH_FINITE_IO_595_binding_invalid")
}
fn injected() -> io::Error {
    io::Error::from_raw_os_error(nix::libc::EIO)
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Fingerprint {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub size: u64,
    pub mtime: i64,
    pub mtime_ns: i64,
    pub ctime: i64,
    pub ctime_ns: i64,
}
impl Fingerprint {
    pub(super) fn of(f: &File) -> io::Result<Self> {
        let m = f.metadata()?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            nlink: m.nlink(),
            size: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
        })
    }
    fn same_inode(&self, b: &Self) -> bool {
        (self.dev, self.ino, self.mode, self.uid, self.gid) == (b.dev, b.ino, b.mode, b.uid, b.gid)
    }
}
fn directory(parent: &File, name: &str) -> io::Result<File> {
    Ok(File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        OpenMode::empty(),
    )?))
}
fn named_file(parent: &File, name: &str) -> io::Result<File> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(invalid());
    }
    Ok(File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        OpenMode::empty(),
    )?))
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum Scope {
    Backup(u64),
    Reconciliation(String, u64),
    Metadata,
}
struct Context {
    root: File,
    container: File,
    scope: Scope,
    record: Record,
    mode: Mode,
}
thread_local! {static CONTEXT:RefCell<Vec<(u64,Arc<Context>)>>=const{RefCell::new(Vec::new())};}
static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);
pub(super) struct ContextGuard(u64);
impl ContextGuard {
    fn push(c: Context) -> Self {
        let id = NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed);
        CONTEXT.with(|x| x.borrow_mut().push((id, Arc::new(c))));
        Self(id)
    }
    pub(super) fn backup(
        root: &File,
        generation_dir: &File,
        generation: u64,
        record: Record,
        mode: Mode,
    ) -> io::Result<Self> {
        let ns = directory(
            &directory(root, "module-recovery-v1")?,
            "backup-lifecycle-v1",
        )?;
        let named = directory(&ns, &format!("{generation:06}"))?;
        if generation == 0
            || !Fingerprint::of(&named)?.same_inode(&Fingerprint::of(generation_dir)?)
        {
            return Err(invalid());
        }
        Ok(Self::push(Context {
            root: root.try_clone()?,
            container: generation_dir.try_clone()?,
            scope: Scope::Backup(generation),
            record,
            mode,
        }))
    }
    pub(super) fn reconciliation(
        proof: &RootProof,
        root: &Path,
        attempt_dir: &File,
        attempt_id: &str,
        sequence: u64,
        record: Record,
        mode: Mode,
    ) -> io::Result<Self> {
        proof
            .with_root(root, |held| {
                Self::reconciliation_held(held, attempt_dir, attempt_id, sequence, record, mode)
            })
            .map_err(|_| invalid())?
    }
    pub(super) fn reconciliation_held(
        root: &File,
        attempt_dir: &File,
        attempt_id: &str,
        sequence: u64,
        record: Record,
        mode: Mode,
    ) -> io::Result<Self> {
        if attempt_id.len() != 64
            || !attempt_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        let named = directory(&directory(root, "module-recovery-v1")?, attempt_id)?;
        if !Fingerprint::of(&named)?.same_inode(&Fingerprint::of(attempt_dir)?) {
            return Err(invalid());
        }
        Ok(Self::push(Context {
            root: root.try_clone()?,
            container: attempt_dir.try_clone()?,
            scope: Scope::Reconciliation(attempt_id.into(), sequence),
            record,
            mode,
        }))
    }
    pub(super) fn metadata(root: &File, parent: &File, record: Record) -> io::Result<Self> {
        Ok(Self::push(Context {
            root: root.try_clone()?,
            container: parent.try_clone()?,
            scope: Scope::Metadata,
            record,
            mode: Mode::Resync,
        }))
    }
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        CONTEXT.with(|x| {
            let mut x = x.borrow_mut();
            if x.last().is_some_and(|q| q.0 == self.0) {
                x.pop();
            } else {
                if let Ok(mut r) = registry().lock() {
                    r.bad_contexts += 1;
                }
                if let Some(i) = x.iter().position(|q| q.0 == self.0) {
                    x.remove(i);
                }
            }
        });
    }
}
fn context() -> Option<Arc<Context>> {
    CONTEXT.with(|x| x.borrow().last().map(|q| q.1.clone()))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Observation {
    pub record: Record,
    pub mode: Mode,
    pub sync_point: Option<SyncPoint>,
    pub root: Fingerprint,
    pub parent: Fingerprint,
    pub target_before: Fingerprint,
    pub target_after: Fingerprint,
    pub name: Option<String>,
    pub scope: String,
    pub delegated: bool,
    pub success: bool,
    pub bytes_written: usize,
    pub short_calls: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Trace {
    pub case: CaseId,
    pub hits: usize,
    pub duplicates: usize,
    pub bad_contexts: usize,
    pub observations: Vec<Observation>,
}
struct Armed {
    case: CaseId,
    root: Fingerprint,
    next_generation: u64,
    prior_attempts: BTreeSet<String>,
    expected_attempt: Option<String>,
    bound: Option<(Scope, Fingerprint, Fingerprint)>,
    hits: usize,
    duplicates: usize,
    observations: Vec<Observation>,
}
#[derive(Default)]
struct Registry {
    armed: Option<Armed>,
    watch: Option<(Fingerprint, Vec<Observation>)>,
    bad_contexts: usize,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}
fn attempts(root: &Path) -> io::Result<BTreeSet<String>> {
    let ns = root.join("module-recovery-v1");
    if !ns.exists() {
        return Ok(BTreeSet::new());
    }
    fs::read_dir(ns)?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .filter(|x: &io::Result<String>| x.as_ref().map_or(true, |n| n.len() == 64))
        .collect()
}
fn next_generation(root: &Path) -> io::Result<u64> {
    let ns = root.join("module-recovery-v1/backup-lifecycle-v1");
    if !ns.exists() {
        return Ok(1);
    }
    let mut maximum = 0;
    for row in fs::read_dir(ns)? {
        let row = row?;
        let n = row.file_name();
        let n = n.to_str().ok_or_else(invalid)?;
        if n.len() == 6 && n.bytes().all(|c| c.is_ascii_digit()) {
            maximum = maximum.max(n.parse::<u64>().map_err(|_| invalid())?);
        }
    }
    Ok(maximum + 1)
}
pub(super) struct FaultArm {
    case: CaseId,
    finished: bool,
}
impl FaultArm {
    pub(super) fn arm(case: CaseId, root: &Path, proof: &RootProof) -> io::Result<Self> {
        let root_fp = proof
            .with_root(root, Fingerprint::of)
            .map_err(|_| invalid())??;
        let before = attempts(root)?;
        let expected_attempt = if matches!(
            case,
            CaseId::A08 | CaseId::A09 | CaseId::A10 | CaseId::B02 | CaseId::B03
        ) {
            if before.len() != 1 {
                return Err(invalid());
            }
            before.first().cloned()
        } else {
            None
        };
        let mut r = registry().lock().map_err(|_| invalid())?;
        if r.armed.is_some() || r.watch.is_some() {
            return Err(invalid());
        }
        r.bad_contexts = 0;
        r.armed = Some(Armed {
            case,
            root: root_fp,
            next_generation: next_generation(root)?,
            prior_attempts: before,
            expected_attempt,
            bound: None,
            hits: 0,
            duplicates: 0,
            observations: Vec::new(),
        });
        Ok(Self {
            case,
            finished: false,
        })
    }
    pub(super) fn finish(mut self) -> io::Result<Trace> {
        let mut r = registry().lock().map_err(|_| invalid())?;
        let a = r.armed.take().ok_or_else(invalid)?;
        self.finished = true;
        let t = Trace {
            case: a.case,
            hits: a.hits,
            duplicates: a.duplicates,
            bad_contexts: r.bad_contexts,
            observations: a.observations,
        };
        if a.case != self.case
            || t.hits != 1
            || t.duplicates != 0
            || t.bad_contexts != 0
            || t.observations.len() != 1
        {
            return Err(invalid());
        }
        let (scope, parent, target) = a.bound.ok_or_else(invalid)?;
        let o = &t.observations[0];
        if o.scope != format!("{scope:?}") || o.parent != parent || o.target_before != target {
            return Err(invalid());
        }
        Ok(t)
    }
}
impl Drop for FaultArm {
    fn drop(&mut self) {
        if !self.finished {
            if let Ok(mut r) = registry().lock() {
                r.armed = None;
            }
        }
    }
}
pub(super) struct WatchGuard;
impl WatchGuard {
    pub(super) fn start(root: &Path, proof: &RootProof) -> io::Result<Self> {
        let fp = proof
            .with_root(root, Fingerprint::of)
            .map_err(|_| invalid())??;
        let mut r = registry().lock().map_err(|_| invalid())?;
        if r.armed.is_some() || r.watch.is_some() {
            return Err(invalid());
        }
        r.bad_contexts = 0;
        r.watch = Some((fp, Vec::new()));
        Ok(Self)
    }
    pub(super) fn finish(self) -> io::Result<Vec<Observation>> {
        let result = {
            let mut r = registry().lock().map_err(|_| invalid())?;
            if r.bad_contexts != 0 {
                Err(invalid())
            } else {
                Ok(r.watch.take().ok_or_else(invalid)?.1)
            }
        };
        result
    }
}
impl Drop for WatchGuard {
    fn drop(&mut self) {
        if let Ok(mut r) = registry().lock() {
            r.watch = None;
        }
    }
}
fn record_name(c: &Context) -> Option<String> {
    match c.record {
        Record::Stage => Some("stage".into()),
        Record::Ready => Some("ready.json".into()),
        Record::Commit => Some("commit.json".into()),
        Record::PublishedManifest => Some("published-manifest.json".into()),
        Record::Requested | Record::CompletionReceipt => match &c.scope {
            Scope::Reconciliation(_, n) => Some(format!("{n:06}.json")),
            _ => None,
        },
        _ => None,
    }
}
fn reserve(
    c: &Context,
    parent: &File,
    target: &File,
    name: Option<&str>,
    point: Option<SyncPoint>,
) -> io::Result<Option<Action>> {
    let root = Fingerprint::of(&c.root)?;
    let parent_fp = Fingerprint::of(parent)?;
    let target_fp = Fingerprint::of(target)?;
    if let Some(n) = name {
        let named = named_file(parent, n)?;
        if Fingerprint::of(&named)? != target_fp {
            return Err(invalid());
        }
    }
    let mut r = registry().lock().map_err(|_| invalid())?;
    let Some(a) = r.armed.as_mut() else {
        return Ok(None);
    };
    if c.mode != Mode::Mutation || !a.root.same_inode(&root) {
        return Ok(None);
    }
    let (record, expected_point, action) = a.case.selection();
    if c.record != record || point != expected_point {
        return Ok(None);
    }
    let valid = match &c.scope {
        Scope::Backup(n) => a.case.backup() && *n == a.next_generation,
        Scope::Reconciliation(id, _) => {
            !a.case.backup()
                && a.expected_attempt
                    .as_ref()
                    .map_or(!a.prior_attempts.contains(id), |want| want == id)
        }
        Scope::Metadata => false,
    };
    if !valid {
        return Err(invalid());
    }
    if a.hits != 0 {
        a.duplicates += 1;
        return Err(invalid());
    }
    if c.record == Record::Requested && !matches!(&c.scope, Scope::Reconciliation(_, 1)) {
        return Err(invalid());
    }
    if let Some(n) = name {
        if record_name(c).as_deref() != Some(n) {
            return Err(invalid());
        }
    }
    if point == Some(SyncPoint::File) {
        let n = record_name(c).ok_or_else(invalid)?;
        if Fingerprint::of(&named_file(&c.container, &n)?)? != target_fp {
            return Err(invalid());
        }
    }
    if point == Some(SyncPoint::AppendRoot) && !target_fp.same_inode(&root) {
        return Err(invalid());
    }
    if point == Some(SyncPoint::RecoveryArchive)
        && !target_fp.same_inode(&Fingerprint::of(&directory(
            &c.container,
            "recovered-original",
        )?)?)
    {
        return Err(invalid());
    }
    if point == Some(SyncPoint::BackupPreparedSource)
        && !target_fp.same_inode(&Fingerprint::of(&directory(&c.root, "module-backups")?)?)
    {
        return Err(invalid());
    }
    if point.is_none()
        && (target_fp.size != 0
            || target_fp.nlink != 1
            || target_fp.mode & (nix::libc::S_IFMT as u32) != (nix::libc::S_IFREG as u32))
    {
        return Err(invalid());
    }
    a.bound = Some((c.scope.clone(), parent_fp, target_fp));
    a.hits += 1;
    Ok(Some(action))
}
fn observation(
    c: &Context,
    parent: &File,
    target: &File,
    before: Fingerprint,
    name: Option<&str>,
    point: Option<SyncPoint>,
    delegated: bool,
    success: bool,
    bytes: usize,
    short_calls: usize,
) -> io::Result<Observation> {
    Ok(Observation {
        record: c.record,
        mode: c.mode,
        sync_point: point,
        root: Fingerprint::of(&c.root)?,
        parent: Fingerprint::of(parent)?,
        target_before: before,
        target_after: Fingerprint::of(target)?,
        name: name.map(str::to_owned),
        scope: format!("{:?}", c.scope),
        delegated,
        success,
        bytes_written: bytes,
        short_calls,
    })
}
fn publish(c: &Context, o: Observation, injected_action: bool) -> io::Result<()> {
    let mut r = registry().lock().map_err(|_| invalid())?;
    if injected_action {
        r.armed
            .as_mut()
            .ok_or_else(invalid)?
            .observations
            .push(o.clone());
    }
    if let Some((root, rows)) = &mut r.watch {
        if root.same_inode(&Fingerprint::of(&c.root)?) {
            if rows.len() >= 16384 {
                return Err(invalid());
            }
            rows.push(o);
        }
    }
    Ok(())
}
struct ShortWriter<'a> {
    file: &'a mut File,
    first: bool,
    limit: usize,
    calls: usize,
    total: usize,
}
impl Write for ShortWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = if self.first {
            self.first = false;
            self.file.write(&buf[..buf.len().min(self.limit)])?
        } else {
            self.file.write(buf)?
        };
        self.calls += 1;
        self.total += n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
pub(super) fn write_all(
    parent: &File,
    name: &str,
    target: &mut File,
    raw: &[u8],
) -> io::Result<()> {
    let Some(c) = context() else {
        return target.write_all(raw);
    };
    let before = Fingerprint::of(target)?;
    let action = reserve(&c, parent, target, Some(name), None)?;
    let (result, bytes, calls) = match action {
        Some(Action::ShortOnce) => {
            if raw.len() < 2 {
                return Err(invalid());
            }
            let mut adapter = ShortWriter {
                file: target,
                first: true,
                limit: (raw.len() / 2).max(1),
                calls: 0,
                total: 0,
            };
            let result = adapter.write_all(raw);
            (result, adapter.total, adapter.calls)
        }
        Some(Action::PrefixThenEio) => {
            if raw.len() < 2 {
                return Err(invalid());
            }
            let prefix = (raw.len() / 2).max(1);
            let result = target.write_all(&raw[..prefix]);
            match result {
                Ok(()) => (Err(injected()), prefix, 0),
                Err(e) => (Err(e), 0, 0),
            }
        }
        Some(Action::SyncEio) => return Err(invalid()),
        None => (target.write_all(raw), raw.len(), 0),
    };
    if action.is_some() {
        let o = observation(
            &c,
            parent,
            target,
            before,
            Some(name),
            None,
            true,
            result.is_ok(),
            bytes,
            calls,
        )?;
        publish(&c, o, true)?;
    }
    result
}
pub(super) fn sync_all(point: SyncPoint, target: &File) -> io::Result<()> {
    let Some(c) = context() else {
        return target.sync_all();
    };
    let before = Fingerprint::of(target)?;
    let action = reserve(&c, &c.container, target, None, Some(point))?;
    let delegated = action.is_none();
    let result = if delegated {
        target.sync_all()
    } else {
        Err(injected())
    };
    let o = observation(
        &c,
        &c.container,
        target,
        before,
        None,
        Some(point),
        delegated,
        result.is_ok(),
        0,
        0,
    )?;
    publish(&c, o, action.is_some())?;
    result
}

mod workers;
