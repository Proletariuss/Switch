//! Checked companion transaction. Independent of production socket_lifetime.
use super::{
    AcceptanceAdmission, Proof,
    checked_fd::{CheckedFd, CloseOps},
    reports::*,
};
// The non-owning syscall wrapper is intentional: nix::fcntl::Flock owns its
// descriptor and unlocks in Drop, which cannot produce checked dispositions.
#[allow(deprecated)]
use nix::{
    errno::Errno,
    fcntl::{AtFlags, FlockArg, OFlag, flock, openat},
    sys::stat::{Mode, fstat, fstatat},
};
use std::{
    cell::RefCell,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    path::PathBuf,
    rc::Rc,
};

const COMPANION: &str = "switch.sock.switch-listener.lock";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Meta {
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
    links: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Meta {
    fn from_stat(m: &nix::sys::stat::FileStat) -> Self {
        Self {
            dev: m.st_dev as u64,
            ino: m.st_ino as u64,
            uid: m.st_uid,
            mode: m.st_mode as u32,
            links: m.st_nlink as u64,
            size: m.st_size as u64,
            mtime: (m.st_mtime, m.st_mtime_nsec),
            ctime: (m.st_ctime, m.st_ctime_nsec),
        }
    }
    fn directory_eq(self, b: Self) -> bool {
        (self.dev, self.ino, self.uid, self.mode) == (b.dev, b.ino, b.uid, b.mode)
    }
    fn directory(self, expected: Self, phase: Phase) -> Result<(), Fault> {
        if self.mode & 0o170000 != 0o040000 {
            return Err(Fault::new(phase, Role::Parent, Kind::Type, None));
        }
        if self.uid != expected.uid {
            return Err(Fault::new(phase, Role::Parent, Kind::Uid, None));
        }
        if self.mode & 0o7777 != 0o700 {
            return Err(Fault::new(phase, Role::Parent, Kind::Mode, None));
        }
        if !self.directory_eq(expected) {
            return Err(Fault::new(phase, Role::Parent, Kind::Identity, None));
        }
        Ok(())
    }
    fn lock(self, uid: u32, phase: Phase) -> Result<(), Fault> {
        for (valid, kind) in [
            (self.mode & 0o170000 == 0o100000, Kind::Type),
            (self.uid == uid, Kind::Uid),
            (self.mode & 0o7777 == 0o600, Kind::Mode),
            (self.links == 1, Kind::Links),
            (self.size == 0, Kind::Size),
        ] {
            if !valid {
                return Err(Fault::new(phase, Role::Lock, kind, None));
            }
        }
        Ok(())
    }
}
enum LockAttempt {
    Held,
    Busy,
    Error(Errno),
    Uncertain(Errno),
}
fn classify_lock(result: Result<(), Errno>) -> LockAttempt {
    match result {
        Ok(()) => LockAttempt::Held,
        Err(Errno::EWOULDBLOCK) => LockAttempt::Busy,
        Err(e @ (Errno::EBADF | Errno::EINVAL | Errno::ENOTSUP)) => LockAttempt::Error(e),
        // Any other errno has no locally established no-lock-effect contract.
        // Attempt one explicit unlock, close everything, and retain uncertainty.
        Err(e) => LockAttempt::Uncertain(e),
    }
}
trait Ops: CloseOps {
    fn checkpoint(&mut self, _: Phase) -> Result<(), Fault> {
        Ok(())
    }
    fn open_parent(&mut self, source: BorrowedFd<'_>) -> Result<OwnedFd, Errno>;
    fn open_lock(&mut self, parent: BorrowedFd<'_>) -> Result<OwnedFd, Errno>;
    fn held(&mut self, fd: BorrowedFd<'_>) -> Result<Meta, Errno>;
    fn named_parent(&mut self, source: BorrowedFd<'_>, basename: &str) -> Result<Meta, Errno>;
    fn named_lock(&mut self, parent: BorrowedFd<'_>) -> Result<Meta, Errno>;
    fn try_lock(&mut self, lock: BorrowedFd<'_>) -> LockAttempt;
    fn unlock(&mut self, lock: BorrowedFd<'_>) -> Result<(), Errno>;
}
struct KernelOps;
impl CloseOps for KernelOps {
    fn close(&mut self, fd: OwnedFd, _: Role) -> Result<(), Errno> {
        nix::unistd::close(fd)
    }
}
#[allow(deprecated)]
impl Ops for KernelOps {
    fn open_parent(&mut self, source: BorrowedFd<'_>) -> Result<OwnedFd, Errno> {
        openat(
            source,
            ".",
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
    }
    fn open_lock(&mut self, parent: BorrowedFd<'_>) -> Result<OwnedFd, Errno> {
        openat(
            parent,
            COMPANION,
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::from_bits_truncate(0o600),
        )
    }
    fn held(&mut self, fd: BorrowedFd<'_>) -> Result<Meta, Errno> {
        fstat(fd).map(|m| Meta::from_stat(&m))
    }
    fn named_parent(&mut self, source: BorrowedFd<'_>, basename: &str) -> Result<Meta, Errno> {
        fstatat(source, basename, AtFlags::AT_SYMLINK_NOFOLLOW).map(|m| Meta::from_stat(&m))
    }
    fn named_lock(&mut self, parent: BorrowedFd<'_>) -> Result<Meta, Errno> {
        fstatat(parent, COMPANION, AtFlags::AT_SYMLINK_NOFOLLOW).map(|m| Meta::from_stat(&m))
    }
    fn try_lock(&mut self, lock: BorrowedFd<'_>) -> LockAttempt {
        classify_lock(flock(lock.as_raw_fd(), FlockArg::LockExclusiveNonblock))
    }
    fn unlock(&mut self, lock: BorrowedFd<'_>) -> Result<(), Errno> {
        flock(lock.as_raw_fd(), FlockArg::Unlock)
    }
}
trait Context {
    fn source_parent(&self) -> Result<BorrowedFd<'_>, Fault>;
    fn named_location(&self) -> Result<(BorrowedFd<'_>, &str), Fault>;
    fn anchor(&self) -> Meta;
    fn validate(&mut self) -> Result<(), Fault>;
}
struct AdmissionContext<'a, P: Proof> {
    admission: &'a mut AcceptanceAdmission,
    proof: &'a P,
    root: PathBuf,
    anchor: Meta,
}
impl<P: Proof> Context for AdmissionContext<'_, P> {
    fn source_parent(&self) -> Result<BorrowedFd<'_>, Fault> {
        self.admission
            .binding
            .held
            .iter()
            .find(|h| h.directory && h.path == self.admission.binding.manifest.data_root)
            .map(|h| h.file.as_fd())
            .ok_or(Fault::new(
                Phase::ParentOpen,
                Role::Parent,
                Kind::Invariant,
                None,
            ))
    }
    fn named_location(&self) -> Result<(BorrowedFd<'_>, &str), Fault> {
        retained_parent(
            &self.admission.binding,
            self.root.to_str().ok_or(Fault::new(
                Phase::ParentNamed,
                Role::Parent,
                Kind::Identity,
                None,
            ))?,
        )
    }
    fn anchor(&self) -> Meta {
        self.anchor
    }
    fn validate(&mut self) -> Result<(), Fault> {
        validate_retained(self.admission, self.proof)
    }
}
fn full_identity(m: &nix::sys::stat::FileStat) -> super::Identity {
    super::Identity {
        dev: m.st_dev as u64,
        ino: m.st_ino as u64,
        uid: m.st_uid,
        mode: m.st_mode as u32,
        links: m.st_nlink as u64,
        size: m.st_size as u64,
        mtime: (m.st_mtime, m.st_mtime_nsec),
        ctime: (m.st_ctime, m.st_ctime_nsec),
    }
}
fn retained_parent<'a>(
    binding: &'a super::RunBinding,
    path: &'a str,
) -> Result<(BorrowedFd<'a>, &'a str), Fault> {
    let fault = || Fault::new(Phase::ContextAfter, Role::Parent, Kind::Identity, None);
    super::canonical(path).map_err(|_| fault())?;
    let (prefix, basename) = path.rsplit_once('/').ok_or_else(fault)?;
    if basename.is_empty() || matches!(basename, "." | "..") || basename.as_bytes().contains(&0) {
        return Err(fault());
    }
    let parent_path = if prefix.is_empty() { "/" } else { prefix };
    let mut parent: Option<&super::Held> = None;
    for candidate in binding.held.iter().filter(|p| p.path == parent_path) {
        if !candidate.directory {
            return Err(fault());
        }
        let current = full_identity(
            &fstat(&candidate.file).map_err(|e| io_error(Phase::ContextAfter, Role::Parent, e))?,
        );
        if current.mode & 0o170000 != 0o040000 || !candidate.identity.directory_eq(&current) {
            return Err(fault());
        }
        if let Some(prior) = parent {
            if !prior.identity.directory_eq(&candidate.identity) {
                return Err(fault());
            }
        } else {
            parent = Some(candidate);
        }
    }
    Ok((parent.ok_or_else(fault)?.file.as_fd(), basename))
}
/// Borrow B1 custody without its runtime_proof/walk temporary opens. Every
/// named lookup is relative to an already held parent FD, never path-only.
fn validate_retained<P: Proof>(
    admission: &mut AcceptanceAdmission,
    proof: &P,
) -> Result<(), Fault> {
    let fault = || Fault::new(Phase::ContextAfter, Role::Parent, Kind::Identity, None);
    if admission.terminal {
        return Err(fault());
    }
    let result = (|| {
        let binding = &admission.binding;
        if binding.held.is_empty() {
            return Err(fault());
        }
        for held in &binding.held {
            let actual = full_identity(
                &fstat(&held.file).map_err(|e| io_error(Phase::ContextAfter, Role::Parent, e))?,
            );
            if !(if held.directory {
                held.identity.directory_eq(&actual)
            } else {
                held.identity == actual
            }) {
                return Err(fault());
            }
            if held.path == "/" {
                if !held.directory {
                    return Err(fault());
                }
                continue;
            }
            // Identical duplicate B1 walks collapse to one parent object;
            // missing or differing held parents are rejected by retained_parent.
            let (parent, basename) = retained_parent(binding, &held.path)?;
            let named = full_identity(
                &fstatat(parent, basename, AtFlags::AT_SYMLINK_NOFOLLOW)
                    .map_err(|e| io_error(Phase::ContextAfter, Role::Parent, e))?,
            );
            if !(if held.directory {
                held.identity.directory_eq(&named)
            } else {
                held.identity == named
            }) {
                return Err(fault());
            }
        }
        if binding.processes
            != super::process_chain(&binding.manifest, proof)
                .map_err(|_| Fault::new(Phase::ContextAfter, Role::Parent, Kind::Context, None))?
        {
            return Err(fault());
        }
        binding
            .validate_environment(proof)
            .map_err(|_| Fault::new(Phase::ContextAfter, Role::Parent, Kind::Context, None))
    })();
    if result.is_err() {
        admission.terminal = true;
    }
    result
}
/// Sole non-test entry requires retained B1 authority; paths/reports/DTOs alone
/// cannot construct a claim. This entry itself has no shipping call site.
fn acquire_for_admission<P: Proof>(
    admission: &mut AcceptanceAdmission,
    proof: &P,
    sink: Sink,
) -> Result<CheckedClaim<KernelOps>, Snapshot> {
    let m = &admission.binding.manifest;
    let h = admission
        .binding
        .held
        .iter()
        .find(|h| h.directory && h.path == m.data_root);
    let Some(h) = h else {
        return Err(Transaction::reject_before_open(
            KernelOps,
            sink,
            Fault::new(Phase::Reserve, Role::Parent, Kind::Invariant, None),
        ));
    };
    let id = &h.identity;
    let anchor = Meta {
        dev: id.dev,
        ino: id.ino,
        uid: id.uid,
        mode: id.mode,
        links: id.links,
        size: id.size,
        mtime: id.mtime,
        ctime: id.ctime,
    };
    let root = PathBuf::from(&m.data_root);
    let mut context = AdmissionContext {
        admission,
        proof,
        root,
        anchor,
    };
    Transaction::acquire(&mut context, KernelOps, sink)
}
struct CheckedClaim<O: Ops> {
    transaction: Transaction<O>,
}
impl<O: Ops> CheckedClaim<O> {
    fn release(&mut self) -> Snapshot {
        self.transaction.finish(None);
        self.transaction.sink.snapshot()
    }
}
struct Transaction<O: Ops> {
    ops: Rc<RefCell<O>>,
    sink: Sink,
    parent: Option<CheckedFd<O>>,
    lock: Option<CheckedFd<O>>,
    tickets: [Ticket; 2],
    finish_permit: Option<FinishPermit>,
    lock_state: LockState,
    unlock_attempted: bool,
    finished: bool,
}
fn io_error(phase: Phase, role: Role, errno: Errno) -> Fault {
    Fault::new(phase, role, Kind::Io, Some(errno))
}
impl<O: Ops> Transaction<O> {
    fn acquire(
        context: &mut impl Context,
        ops: O,
        sink: Sink,
    ) -> Result<CheckedClaim<O>, Snapshot> {
        let mut tx = Self::reserved(ops, sink)?;
        let result = tx.steps(context).and_then(|_| tx.sink.authority_ready());
        match result {
            Ok(()) => Ok(CheckedClaim { transaction: tx }),
            Err(error) => {
                tx.finish(Some(error));
                Err(tx.sink.snapshot())
            }
        }
    }
    fn reject_before_open(ops: O, sink: Sink, fault: Fault) -> Snapshot {
        match Self::reserved(ops, sink) {
            Ok(mut tx) => {
                tx.finish(Some(fault));
                tx.sink.snapshot()
            }
            Err(view) => view,
        }
    }
    fn reserved(ops: O, sink: Sink) -> Result<Self, Snapshot> {
        // B2 maximum: 2 owned FDs; 3 cleanup calls (unlock, lock-close,
        // parent-close); 15 acquisition events and a separate 3-slot cleanup
        // ledger; one primary fault and 8 secondary faults.
        // fstat/fstatat/metadata/flock create no extra FD. No socket/dup here.
        let reservation = match sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS) {
            Ok(t) => t,
            Err(e) => {
                // Failed Fresh reservation has atomically sealed its capacity
                // primary. An Active owner, if any, alone may finish cleanup.
                let _ = e;
                return Err(sink.snapshot());
            }
        };
        Ok(Self {
            ops: Rc::new(RefCell::new(ops)),
            sink,
            parent: None,
            lock: None,
            tickets: reservation.tickets,
            finish_permit: Some(reservation.finish),
            lock_state: LockState::NotAttempted,
            unlock_attempted: false,
            finished: false,
        })
    }
    fn step(&self, phase: Phase, role: Role) -> Result<(), Fault> {
        self.sink.step(phase, role)?;
        self.ops.borrow_mut().checkpoint(phase)?;
        self.sink.permit_operation().map_err(|mut error| {
            error.phase = phase;
            error.role = role;
            error
        })
    }
    fn parent_fd(&self) -> Result<BorrowedFd<'_>, Fault> {
        self.parent
            .as_ref()
            .and_then(CheckedFd::fd)
            .ok_or(Fault::new(
                Phase::Ledger,
                Role::Parent,
                Kind::Invariant,
                None,
            ))
    }
    fn lock_fd(&self) -> Result<BorrowedFd<'_>, Fault> {
        self.lock.as_ref().and_then(CheckedFd::fd).ok_or(Fault::new(
            Phase::Ledger,
            Role::Lock,
            Kind::Invariant,
            None,
        ))
    }
    fn check_parent(
        &self,
        context: &impl Context,
        held_phase: Phase,
        named_phase: Phase,
    ) -> Result<(), Fault> {
        self.step(held_phase, Role::Parent)?;
        let held = self
            .ops
            .borrow_mut()
            .held(self.parent_fd()?)
            .map_err(|e| io_error(held_phase, Role::Parent, e))?;
        held.directory(context.anchor(), held_phase)?;
        self.step(named_phase, Role::Parent)?;
        let (source, basename) = context.named_location()?;
        let named = self
            .ops
            .borrow_mut()
            .named_parent(source, basename)
            .map_err(|e| io_error(named_phase, Role::Parent, e))?;
        named.directory(context.anchor(), named_phase)
    }
    fn check_lock(&self, uid: u32, held_phase: Phase, named_phase: Phase) -> Result<Meta, Fault> {
        self.step(held_phase, Role::Lock)?;
        let held = self
            .ops
            .borrow_mut()
            .held(self.lock_fd()?)
            .map_err(|e| io_error(held_phase, Role::Lock, e))?;
        held.lock(uid, held_phase)?;
        self.step(named_phase, Role::Lock)?;
        let named = self
            .ops
            .borrow_mut()
            .named_lock(self.parent_fd()?)
            .map_err(|e| io_error(named_phase, Role::Lock, e))?;
        named.lock(uid, named_phase)?;
        if held != named {
            return Err(Fault::new(named_phase, Role::Lock, Kind::Identity, None));
        }
        Ok(held)
    }
    fn steps(&mut self, context: &mut impl Context) -> Result<(), Fault> {
        self.step(Phase::ContextBefore, Role::Parent)?;
        context.validate().map_err(|mut e| {
            e.phase = Phase::ContextBefore;
            e
        })?;
        self.step(Phase::ParentOpen, Role::Parent)?;
        let fd = self
            .ops
            .borrow_mut()
            .open_parent(context.source_parent()?)
            .map_err(|e| io_error(Phase::ParentOpen, Role::Parent, e))?;
        let raw = fd.as_raw_fd();
        self.parent = Some(CheckedFd::adopt_pending(
            fd,
            self.tickets[0],
            self.sink.clone(),
            Rc::clone(&self.ops),
        ));
        self.finish_permit
            .as_ref()
            .expect("reserved completion right")
            .adopt_pending(self.tickets[0], raw);
        self.sink.acquired(self.tickets[0], raw)?;
        self.check_parent(context, Phase::ParentStat, Phase::ParentNamed)?;
        self.step(Phase::LockOpen, Role::Lock)?;
        let fd = self
            .ops
            .borrow_mut()
            .open_lock(self.parent_fd()?)
            .map_err(|e| io_error(Phase::LockOpen, Role::Lock, e))?;
        let raw = fd.as_raw_fd();
        self.lock = Some(CheckedFd::adopt_pending(
            fd,
            self.tickets[1],
            self.sink.clone(),
            Rc::clone(&self.ops),
        ));
        self.finish_permit
            .as_ref()
            .expect("reserved completion right")
            .adopt_pending(self.tickets[1], raw);
        self.sink.acquired(self.tickets[1], raw)?;
        let initial = self.check_lock(context.anchor().uid, Phase::LockStat, Phase::LockNamed)?;
        self.step(Phase::TryLock, Role::Lock)?;
        let attempt = self.ops.borrow_mut().try_lock(self.lock_fd()?);
        match attempt {
            LockAttempt::Held => {
                self.lock_state = LockState::Held;
                self.sink.lock(LockState::Held)?;
            }
            LockAttempt::Busy => {
                self.lock_state = LockState::NotHeld;
                self.sink.lock(LockState::NotHeld)?;
                return Err(Fault::new(Phase::TryLock, Role::Lock, Kind::Busy, None));
            }
            LockAttempt::Error(e) => {
                self.lock_state = LockState::NotHeld;
                self.sink.lock(LockState::NotHeld)?;
                return Err(io_error(Phase::TryLock, Role::Lock, e));
            }
            LockAttempt::Uncertain(e) => {
                self.lock_state = LockState::Uncertain;
                self.sink.lock(LockState::Uncertain)?;
                return Err(Fault::new(
                    Phase::TryLock,
                    Role::Lock,
                    Kind::Uncertain,
                    Some(e),
                ));
            }
        }
        // Held state is owned and recorded before any fallible post-lock step.
        self.step(Phase::PostLock, Role::Lock)?;
        self.check_parent(context, Phase::PostParentStat, Phase::PostParentNamed)?;
        let current = self.check_lock(
            context.anchor().uid,
            Phase::PostLockStat,
            Phase::PostLockNamed,
        )?;
        if initial != current {
            return Err(Fault::new(
                Phase::PostLockStat,
                Role::Lock,
                Kind::Identity,
                None,
            ));
        }
        self.step(Phase::ContextAfter, Role::Parent)?;
        context.validate()?;
        self.step(Phase::Committed, Role::Lock)
    }
    /// Single checked abort/release outlet. No ? can skip a later close.
    fn finish(&mut self, primary: Option<Fault>) {
        if self.finished {
            if primary.is_some() {
                self.sink.request_stop();
            }
            return;
        }
        if let Some(fault) = primary {
            self.sink.primary(fault);
        }
        if matches!(self.lock_state, LockState::Held | LockState::Uncertain)
            && !self.unlock_attempted
        {
            self.unlock_attempted = true;
            let mut guard = self.sink.call_guard(Phase::Unlock, Role::Lock);
            let uncertain = self.lock_state == LockState::Uncertain;
            let result = match self.lock_fd() {
                Ok(fd) => self.ops.borrow_mut().unlock(fd),
                Err(_) => Err(Errno::EBADF),
            };
            guard.complete();
            self.sink.cleanup(Cleanup {
                resource: 1,
                role: Role::Lock,
                phase: Phase::Unlock,
                action: Action::Unlock,
                outcome: match result {
                    Ok(()) => Outcome::Success,
                    Err(e) => Outcome::Error(e),
                },
            });
            self.lock_state = match result {
                Ok(()) if uncertain => LockState::ReleasedUncertain,
                Ok(()) => LockState::Released,
                Err(_) => LockState::ReleaseFailed,
            };
            let _ = self.sink.lock(self.lock_state);
        } else if matches!(self.lock_state, LockState::Held | LockState::Uncertain) {
            self.sink.fallback();
        }
        if let Some(fd) = self.lock.as_mut() {
            fd.close_once();
        }
        if let Some(fd) = self.parent.as_mut() {
            fd.close_once();
        }
        self.finished = true;
        if let Some(permit) = self.finish_permit.take() {
            permit.seal();
        }
    }
}
impl<O: Ops> Drop for Transaction<O> {
    #[allow(deprecated)]
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.sink.fallback();
        if matches!(self.lock_state, LockState::Held | LockState::Uncertain)
            && !self.unlock_attempted
        {
            self.unlock_attempted = true;
            let result = self
                .lock_fd()
                .ok()
                .map(|fd| flock(fd.as_raw_fd(), FlockArg::Unlock));
            self.sink.cleanup(Cleanup {
                resource: 1,
                role: Role::Lock,
                phase: Phase::Unlock,
                action: Action::Unlock,
                outcome: Outcome::Unchecked(result.and_then(Result::err)),
            });
            let _ = self.sink.lock(LockState::ReleaseFailed);
        }
        if let Some(fd) = self.lock.as_mut() {
            fd.fallback_close();
        }
        if let Some(fd) = self.parent.as_mut() {
            fd.fallback_close();
        }
        if let Some(permit) = self.finish_permit.take() {
            permit.seal();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::fcntl::open;
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        panic::{AssertUnwindSafe, catch_unwind},
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        Open(Role),
        Stat(Phase),
        TryLock,
        Unlock,
        Close(Role, i32),
    }
    #[derive(Clone, Copy)]
    enum Mutation {
        Type,
        Uid,
        Mode,
        Links,
        Size,
        Inode,
    }
    #[derive(Clone, Copy)]
    enum Attempt {
        Busy,
        Error,
        UncertainHeld,
        UncertainNotHeld,
    }
    #[derive(Default)]
    struct Trace {
        calls: Vec<Call>,
        observations: Vec<(Phase, Snapshot)>,
        replacement: Option<OwnedFd>,
    }
    #[derive(Default)]
    struct Config {
        error: Option<Phase>,
        panic: Option<Phase>,
        mutation: Option<(Phase, Mutation)>,
        attempt: Option<Attempt>,
        unlock_error: bool,
        close_errors: Vec<(Role, Errno)>,
        reuse: Option<Role>,
        reuse_path: PathBuf,
        drift_file: Option<PathBuf>,
        seal_after_open: Option<Role>,
        poison_after_open: Option<Role>,
        seal_checkpoint: Option<Phase>,
        poison_after_flock: bool,
    }
    struct Injected {
        kernel: KernelOps,
        config: Config,
        trace: Rc<RefCell<Trace>>,
        sink: Sink,
        current: Phase,
    }
    impl Injected {
        fn new(config: Config, sink: Sink) -> (Self, Rc<RefCell<Trace>>) {
            let trace = Rc::new(RefCell::new(Trace::default()));
            (
                Self {
                    kernel: KernelOps,
                    config,
                    trace: Rc::clone(&trace),
                    sink,
                    current: Phase::Reserve,
                },
                trace,
            )
        }
        fn error(&self) -> Result<(), Errno> {
            if self.config.error == Some(self.current) {
                Err(Errno::EACCES)
            } else {
                Ok(())
            }
        }
        fn after_open(&self, role: Role) {
            if self.config.seal_after_open == Some(role) {
                self.sink.request_stop();
            }
            if self.config.poison_after_open == Some(role) {
                self.sink
                    .primary(Fault::new(Phase::Ledger, role, Kind::Invariant, None));
            }
        }
        fn altered(&self, mut m: Meta) -> Meta {
            if let Some((phase, change)) = self.config.mutation {
                if phase == self.current {
                    match change {
                        Mutation::Type => m.mode = (m.mode & 0o7777) | 0o010000,
                        Mutation::Uid => m.uid = m.uid.wrapping_add(1),
                        Mutation::Mode => m.mode |= 0o020,
                        Mutation::Links => m.links += 1,
                        Mutation::Size => m.size += 1,
                        Mutation::Inode => m.ino += 1,
                    }
                }
            }
            m
        }
    }
    impl CloseOps for Injected {
        fn close(&mut self, fd: OwnedFd, role: Role) -> Result<(), Errno> {
            let phase = match role {
                Role::Lock => Phase::LockClose,
                Role::Parent => Phase::ParentClose,
            };
            let raw = fd.as_raw_fd();
            self.trace.borrow_mut().calls.push(Call::Close(role, raw));
            if self.config.panic == Some(phase) {
                panic!("injected close panic");
            }
            self.kernel.close(fd, role)?;
            if self.config.reuse == Some(role) {
                let replacement = open(
                    &self.config.reuse_path,
                    OFlag::O_RDONLY | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )
                .unwrap();
                assert_eq!(
                    replacement.as_raw_fd(),
                    raw,
                    "test must actually reuse the consumed fd number"
                );
                self.trace.borrow_mut().replacement = Some(replacement);
            }
            match self.config.close_errors.iter().find(|(r, _)| *r == role) {
                Some((_, e)) => Err(*e),
                None => Ok(()),
            }
        }
    }
    impl Ops for Injected {
        fn checkpoint(&mut self, phase: Phase) -> Result<(), Fault> {
            self.current = phase;
            if self.config.seal_checkpoint == Some(phase) {
                self.sink.request_stop();
            }
            if phase == Phase::PostLock {
                if let Some(path) = &self.config.drift_file {
                    fs::write(path, b"injected image drift").unwrap();
                }
            }
            self.trace
                .borrow_mut()
                .observations
                .push((phase, self.sink.snapshot()));
            if self.config.panic == Some(phase) {
                panic!("injected checkpoint panic");
            }
            // State-only boundaries have no following syscall to inject into.
            if self.config.error == Some(phase)
                && matches!(
                    phase,
                    Phase::ContextBefore | Phase::PostLock | Phase::ContextAfter | Phase::Committed
                )
            {
                return Err(Fault::new(
                    phase,
                    Role::Parent,
                    Kind::Context,
                    Some(Errno::EIO),
                ));
            }
            Ok(())
        }
        fn open_parent(&mut self, source: BorrowedFd<'_>) -> Result<OwnedFd, Errno> {
            self.trace.borrow_mut().calls.push(Call::Open(Role::Parent));
            self.error()?;
            let fd = self.kernel.open_parent(source)?;
            self.after_open(Role::Parent);
            Ok(fd)
        }
        fn open_lock(&mut self, parent: BorrowedFd<'_>) -> Result<OwnedFd, Errno> {
            self.trace.borrow_mut().calls.push(Call::Open(Role::Lock));
            self.error()?;
            let fd = self.kernel.open_lock(parent)?;
            self.after_open(Role::Lock);
            Ok(fd)
        }
        fn held(&mut self, fd: BorrowedFd<'_>) -> Result<Meta, Errno> {
            self.trace.borrow_mut().calls.push(Call::Stat(self.current));
            self.error()?;
            self.kernel.held(fd).map(|m| self.altered(m))
        }
        fn named_parent(&mut self, source: BorrowedFd<'_>, basename: &str) -> Result<Meta, Errno> {
            self.trace.borrow_mut().calls.push(Call::Stat(self.current));
            self.error()?;
            self.kernel
                .named_parent(source, basename)
                .map(|m| self.altered(m))
        }
        fn named_lock(&mut self, parent: BorrowedFd<'_>) -> Result<Meta, Errno> {
            self.trace.borrow_mut().calls.push(Call::Stat(self.current));
            self.error()?;
            self.kernel.named_lock(parent).map(|m| self.altered(m))
        }
        fn try_lock(&mut self, fd: BorrowedFd<'_>) -> LockAttempt {
            self.trace.borrow_mut().calls.push(Call::TryLock);
            if self.config.poison_after_flock {
                let result = self.kernel.try_lock(fd);
                self.sink.primary(Fault::new(
                    Phase::TryLock,
                    Role::Lock,
                    Kind::Invariant,
                    None,
                ));
                return result;
            }
            if self.error().is_err() {
                return LockAttempt::Error(Errno::EACCES);
            }
            match self.config.attempt {
                Some(Attempt::Busy) => LockAttempt::Busy,
                Some(Attempt::Error) => LockAttempt::Error(Errno::EIO),
                Some(Attempt::UncertainNotHeld) => LockAttempt::Uncertain(Errno::EINTR),
                Some(Attempt::UncertainHeld) => {
                    assert!(matches!(self.kernel.try_lock(fd), LockAttempt::Held));
                    LockAttempt::Uncertain(Errno::EINTR)
                }
                None => self.kernel.try_lock(fd),
            }
        }
        fn unlock(&mut self, fd: BorrowedFd<'_>) -> Result<(), Errno> {
            self.trace.borrow_mut().calls.push(Call::Unlock);
            if self.config.panic == Some(Phase::Unlock) {
                panic!("injected unlock panic");
            }
            if self.config.unlock_error {
                return Err(Errno::EIO);
            }
            self.kernel.unlock(fd)
        }
    }
    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = PathBuf::from(std::env::var_os("G8_FIXTURE_ROOT").expect("isolated root"))
                .join(format!(
                    "b2-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(root.join("replacement-fixture"), b"fixture").unwrap();
            Self { root }
        }
        fn context(&self) -> FixtureContext {
            let anchor = test_meta(&self.root);
            FixtureContext {
                root: self.root.clone(),
                held_root: std::fs::File::open(&self.root).unwrap(),
                held_parent: std::fs::File::open(self.root.parent().unwrap()).unwrap(),
                anchor,
                calls: 0,
                fail_at: None,
            }
        }
    }
    struct FixtureContext {
        root: PathBuf,
        held_root: std::fs::File,
        held_parent: std::fs::File,
        anchor: Meta,
        calls: usize,
        fail_at: Option<usize>,
    }
    impl Context for FixtureContext {
        fn source_parent(&self) -> Result<BorrowedFd<'_>, Fault> {
            Ok(self.held_root.as_fd())
        }
        fn named_location(&self) -> Result<(BorrowedFd<'_>, &str), Fault> {
            Ok((
                self.held_parent.as_fd(),
                self.root.file_name().unwrap().to_str().unwrap(),
            ))
        }
        fn anchor(&self) -> Meta {
            self.anchor
        }
        fn validate(&mut self) -> Result<(), Fault> {
            self.calls += 1;
            if self.fail_at == Some(self.calls) {
                Err(Fault::new(
                    Phase::ContextAfter,
                    Role::Parent,
                    Kind::Context,
                    None,
                ))
            } else {
                Ok(())
            }
        }
    }
    fn failed<O: Ops>(r: Result<CheckedClaim<O>, Snapshot>) -> Snapshot {
        match r {
            Err(e) => e,
            Ok(mut c) => {
                c.release();
                panic!("expected refusal");
            }
        }
    }
    fn count(trace: &Rc<RefCell<Trace>>, call: Call) -> usize {
        trace.borrow().calls.iter().filter(|v| **v == call).count()
    }
    fn closes(trace: &Rc<RefCell<Trace>>, role: Role) -> usize {
        trace
            .borrow()
            .calls
            .iter()
            .filter(|c| matches!(c,Call::Close(r,_)if *r==role))
            .count()
    }
    fn fd_count() -> usize {
        fs::read_dir("/dev/fd").unwrap().count()
    }
    // Test-fixture observations are outside the checked transaction. Product
    // named checks above exclusively use held-parent fstatat, never this helper.
    fn test_meta(path: &Path) -> Meta {
        let m = fs::symlink_metadata(path).unwrap();
        Meta {
            dev: m.dev(),
            ino: m.ino(),
            uid: m.uid(),
            mode: m.mode(),
            links: m.nlink(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }

    #[test]
    fn ab1_parent_and_lock_open_metadata_named_errors_close_every_acquired_fd() {
        for phase in [
            Phase::ContextBefore,
            Phase::ParentOpen,
            Phase::ParentStat,
            Phase::ParentNamed,
            Phase::LockOpen,
            Phase::LockStat,
            Phase::LockNamed,
            Phase::TryLock,
            Phase::PostLock,
            Phase::PostParentStat,
            Phase::PostParentNamed,
            Phase::PostLockStat,
            Phase::PostLockNamed,
            Phase::ContextAfter,
            Phase::Committed,
        ] {
            let fixture = Fixture::new();
            let mut context = fixture.context();
            let sink = Sink::new();
            let (ops, trace) = Injected::new(
                Config {
                    error: Some(phase),
                    ..Config::default()
                },
                sink.clone(),
            );
            let report = failed(Transaction::acquire(&mut context, ops, sink.clone()));
            assert_eq!(report.primary.unwrap().phase, phase);
            assert!(!report.checked_clean());
            assert!(report.resources.iter().all(|r| matches!(
                r.disposition,
                Disposition::ReservedUnused | Disposition::Closed
            )));
            assert!(closes(&trace, Role::Parent) <= 1 && closes(&trace, Role::Lock) <= 1);
            let held = matches!(
                phase,
                Phase::PostLock
                    | Phase::PostParentStat
                    | Phase::PostParentNamed
                    | Phase::PostLockStat
                    | Phase::PostLockNamed
                    | Phase::ContextAfter
                    | Phase::Committed
            );
            assert_eq!(count(&trace, Call::Unlock), usize::from(held), "{phase:?}");
            for (at, view) in &trace.borrow().observations {
                if *at == Phase::ParentStat {
                    assert_eq!(view.resources[0].disposition, Disposition::Owned);
                }
                if *at == Phase::LockStat {
                    assert_eq!(view.resources[1].disposition, Disposition::Owned);
                }
                if *at == Phase::PostLock {
                    assert_eq!(view.lock, LockState::Held);
                }
            }
        }
    }
    #[test]
    fn ab1_metadata_type_uid_mode_link_size_and_named_identity_reject() {
        for phase in [
            Phase::ParentStat,
            Phase::ParentNamed,
            Phase::PostParentStat,
            Phase::PostParentNamed,
        ] {
            for change in [
                Mutation::Type,
                Mutation::Uid,
                Mutation::Mode,
                Mutation::Inode,
            ] {
                let f = Fixture::new();
                let sink = Sink::new();
                let (ops, trace) = Injected::new(
                    Config {
                        mutation: Some((phase, change)),
                        ..Config::default()
                    },
                    sink.clone(),
                );
                let report = failed(Transaction::acquire(&mut f.context(), ops, sink));
                assert_eq!(report.primary.unwrap().phase, phase);
                assert_eq!(closes(&trace, Role::Parent), 1);
                assert!(!report.checked_clean());
            }
        }
        for phase in [
            Phase::LockStat,
            Phase::LockNamed,
            Phase::PostLockStat,
            Phase::PostLockNamed,
        ] {
            for change in [
                Mutation::Type,
                Mutation::Uid,
                Mutation::Mode,
                Mutation::Links,
                Mutation::Size,
                Mutation::Inode,
            ] {
                let f = Fixture::new();
                let sink = Sink::new();
                let (ops, trace) = Injected::new(
                    Config {
                        mutation: Some((phase, change)),
                        ..Config::default()
                    },
                    sink.clone(),
                );
                let report = failed(Transaction::acquire(&mut f.context(), ops, sink));
                assert!(!report.checked_clean());
                assert_eq!(closes(&trace, Role::Lock), 1);
                assert_eq!(closes(&trace, Role::Parent), 1);
            }
        }
    }
    #[test]
    fn ab1_try_lock_busy_error_uncertain_and_held_have_disjoint_cleanup() {
        assert!(matches!(classify_lock(Ok(())), LockAttempt::Held));
        assert!(matches!(
            classify_lock(Err(Errno::EWOULDBLOCK)),
            LockAttempt::Busy
        ));
        for e in [Errno::EBADF, Errno::EINVAL, Errno::ENOTSUP] {
            assert!(matches!(classify_lock(Err(e)),LockAttempt::Error(actual)if actual==e));
        }
        for e in [Errno::EINTR, Errno::EIO, Errno::ENOLCK] {
            assert!(matches!(classify_lock(Err(e)),LockAttempt::Uncertain(actual)if actual==e));
        }
        for attempt in [
            Attempt::Busy,
            Attempt::Error,
            Attempt::UncertainHeld,
            Attempt::UncertainNotHeld,
        ] {
            let f = Fixture::new();
            let sink = Sink::new();
            let (ops, trace) = Injected::new(
                Config {
                    attempt: Some(attempt),
                    ..Config::default()
                },
                sink.clone(),
            );
            let report = failed(Transaction::acquire(&mut f.context(), ops, sink));
            let uncertain = matches!(attempt, Attempt::UncertainHeld | Attempt::UncertainNotHeld);
            assert_eq!(count(&trace, Call::Unlock), usize::from(uncertain));
            assert_eq!(
                report.lock,
                if uncertain {
                    LockState::ReleasedUncertain
                } else {
                    LockState::NotHeld
                }
            );
            assert_eq!(
                report.primary.unwrap().kind,
                match attempt {
                    Attempt::Busy => Kind::Busy,
                    Attempt::Error => Kind::Io,
                    _ => Kind::Uncertain,
                }
            );
            assert!(!report.checked_clean());
            assert_eq!(closes(&trace, Role::Lock), 1);
            assert_eq!(closes(&trace, Role::Parent), 1);
        }
    }
    #[test]
    fn ab1_held_validation_or_context_failure_accumulates_all_cleanup_errors() {
        for phase in [
            Phase::PostLock,
            Phase::PostParentStat,
            Phase::PostLockNamed,
            Phase::ContextAfter,
        ] {
            for mask in 0..8 {
                let f = Fixture::new();
                let sink = Sink::new();
                let mut errors = Vec::new();
                if mask & 2 != 0 {
                    errors.push((Role::Lock, Errno::EINTR));
                }
                if mask & 4 != 0 {
                    errors.push((Role::Parent, Errno::EIO));
                }
                let (ops, trace) = Injected::new(
                    Config {
                        error: Some(phase),
                        unlock_error: mask & 1 != 0,
                        close_errors: errors,
                        ..Config::default()
                    },
                    sink.clone(),
                );
                let report = failed(Transaction::acquire(&mut f.context(), ops, sink));
                let cleanup: Vec<_> = report.cleanup.into_iter().flatten().collect();
                assert_eq!(cleanup.len(), 3);
                assert_eq!(
                    (cleanup[0].phase, cleanup[1].phase, cleanup[2].phase),
                    (Phase::Unlock, Phase::LockClose, Phase::ParentClose)
                );
                assert_eq!(
                    cleanup
                        .iter()
                        .filter(|c| matches!(c.outcome, Outcome::Error(_)))
                        .count(),
                    (mask as u32).count_ones() as usize
                );
                assert_eq!(count(&trace, Call::Unlock), 1);
                assert_eq!(closes(&trace, Role::Lock), 1);
                assert_eq!(closes(&trace, Role::Parent), 1);
                assert_eq!(report.primary.unwrap().phase, phase);
                assert!(!report.checked_clean());
            }
        }
        for fail in [1, 2] {
            let f = Fixture::new();
            let mut context = f.context();
            context.fail_at = Some(fail);
            let sink = Sink::new();
            let report = failed(Transaction::acquire(&mut context, KernelOps, sink));
            assert_eq!(
                report.primary.unwrap().phase,
                if fail == 1 {
                    Phase::ContextBefore
                } else {
                    Phase::ContextAfter
                }
            );
            assert_eq!(
                report.resources[1].disposition,
                if fail == 1 {
                    Disposition::ReservedUnused
                } else {
                    Disposition::Closed
                }
            );
        }
    }
    #[test]
    fn ab1_clean_release_is_once_only_and_repeat_abort_never_reoperates() {
        let f = Fixture::new();
        let sink = Sink::new();
        let (ops, trace) = Injected::new(Config::default(), sink.clone());
        let mut claim = Transaction::acquire(&mut f.context(), ops, sink.clone()).unwrap();
        assert!(!sink.snapshot().checked_clean());
        assert!(claim.release().checked_clean());
        let calls = trace.borrow().calls.clone();
        for _ in 0..8 {
            assert!(claim.release().checked_clean());
        }
        assert_eq!(trace.borrow().calls, calls);
        claim.transaction.finish(Some(Fault::new(
            Phase::ContextAfter,
            Role::Parent,
            Kind::Context,
            None,
        )));
        assert!(!sink.snapshot().checked_clean());
        assert_eq!(trace.borrow().calls, calls);
    }
    #[test]
    fn ab1_report_missing_duplicate_unknown_or_mismatched_cleanup_cannot_be_clean() {
        let f = Fixture::new();
        let mut claim = Transaction::acquire(&mut f.context(), KernelOps, Sink::new()).unwrap();
        let clean = claim.release();
        assert!(clean.checked_clean());
        for index in 0..3 {
            let mut missing = clean;
            missing.cleanup[index] = None;
            assert!(!missing.checked_clean());
            let mut unknown = clean;
            unknown.cleanup[index].as_mut().unwrap().resource = 99;
            assert!(!unknown.checked_clean());
            let mut wrong_role = clean;
            let row = wrong_role.cleanup[index].as_mut().unwrap();
            row.role = if row.role == Role::Parent {
                Role::Lock
            } else {
                Role::Parent
            };
            assert!(!wrong_role.checked_clean());
            let mut unknown_outcome = clean;
            unknown_outcome.cleanup[index].as_mut().unwrap().outcome = Outcome::Unchecked(None);
            assert!(!unknown_outcome.checked_clean());
        }
        let mut duplicate = clean;
        duplicate.cleanup[0] = duplicate.cleanup[1];
        assert!(!duplicate.checked_clean());
        let mut reordered = clean;
        reordered.cleanup.swap(0, 1);
        assert!(!reordered.checked_clean());
        let mut hidden_held = clean;
        hidden_held.lock = LockState::NotAttempted;
        assert!(!hidden_held.checked_clean());
        let mut no_reservation = clean;
        no_reservation.reserved = false;
        assert!(!no_reservation.checked_clean());
        let mut uncertain = clean;
        uncertain.ever_uncertain = true;
        assert!(!uncertain.checked_clean());
    }
    #[test]
    fn ab1_eintr_close_consumes_owner_before_actual_fd_number_reuse() {
        for role in [Role::Lock, Role::Parent] {
            let f = Fixture::new();
            let sink = Sink::new();
            let (ops, trace) = Injected::new(
                Config {
                    close_errors: vec![(role, Errno::EINTR)],
                    reuse: Some(role),
                    reuse_path: f.root.join("replacement-fixture"),
                    ..Config::default()
                },
                sink.clone(),
            );
            let mut context = f.context();
            let mut claim = Transaction::acquire(&mut context, ops, sink.clone()).unwrap();
            let report = claim.release();
            assert!(!report.checked_clean());
            assert!(fstat(trace.borrow().replacement.as_ref().unwrap()).is_ok());
            for _ in 0..4 {
                claim.release();
            }
            drop(claim);
            assert!(fstat(trace.borrow().replacement.as_ref().unwrap()).is_ok());
            assert_eq!(closes(&trace, role), 1);
            let replacement = trace.borrow_mut().replacement.take().unwrap();
            nix::unistd::close(replacement).unwrap();
        }
    }
    #[test]
    fn ab1_panic_early_return_and_cleanup_panics_are_sticky_unchecked() {
        let baseline = fd_count();
        for phase in [
            Phase::ParentStat,
            Phase::LockStat,
            Phase::PostLock,
            Phase::ContextAfter,
        ] {
            let f = Fixture::new();
            let sink = Sink::new();
            let (ops, _) = Injected::new(
                Config {
                    panic: Some(phase),
                    ..Config::default()
                },
                sink.clone(),
            );
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = Transaction::acquire(&mut f.context(), ops, sink.clone());
                }))
                .is_err()
            );
            let view = sink.snapshot();
            assert!(view.unchecked && !view.checked_clean());
            assert!(view.primary.is_some());
        }
        for phase in [Phase::Unlock, Phase::LockClose, Phase::ParentClose] {
            let f = Fixture::new();
            let sink = Sink::new();
            let (ops, trace) = Injected::new(
                Config {
                    panic: Some(phase),
                    ..Config::default()
                },
                sink.clone(),
            );
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    let mut c = Transaction::acquire(&mut f.context(), ops, sink.clone()).unwrap();
                    c.release();
                }))
                .is_err()
            );
            assert!(sink.snapshot().unchecked && !sink.snapshot().checked_clean());
            if matches!(phase, Phase::LockClose | Phase::ParentClose) {
                let index = if phase == Phase::LockClose { 1 } else { 0 };
                assert_eq!(
                    sink.snapshot().resources[index].disposition,
                    Disposition::Unknown
                );
            }
            assert!(count(&trace, Call::Unlock) <= 1);
            assert!(closes(&trace, Role::Lock) <= 1 && closes(&trace, Role::Parent) <= 1);
        }
        let f = Fixture::new();
        let sink = Sink::new();
        {
            let _unreleased =
                Transaction::acquire(&mut f.context(), KernelOps, sink.clone()).unwrap();
        }
        assert!(sink.snapshot().unchecked && !sink.snapshot().checked_clean());
        assert_eq!(fd_count(), baseline);
    }
    #[test]
    fn ab1_reservation_overflow_duplicate_unknown_and_missing_dispositions_fail_closed() {
        for budget in [
            (3, CLEANUP, EVENTS, FAULTS),
            (RESOURCES, 2, EVENTS, FAULTS),
            (RESOURCES, CLEANUP, 31, FAULTS),
            (RESOURCES, CLEANUP, EVENTS, 9),
        ] {
            let sink = Sink::new();
            assert!(
                sink.reserve(budget.0, budget.1, budget.2, budget.3)
                    .is_err()
            );
            sink.request_stop();
            assert!(!sink.snapshot().checked_clean());
        }
        let f = Fixture::new();
        let sink = Sink::new();
        sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        let (ops, trace) = Injected::new(Config::default(), sink.clone());
        let report = failed(Transaction::acquire(&mut f.context(), ops, sink));
        assert!(report.overflow && !report.checked_clean());
        assert!(trace.borrow().calls.is_empty());
        let sink = Sink::new();
        let tickets = sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        let _ = sink.acquired(tickets[0], 123);
        // Establish the capacity fault while still active. Sealed late writes
        // now leave the old record untouched and only stick unchecked.
        assert!(
            sink.reserve(RESOURCES + 1, CLEANUP, EVENTS, FAULTS)
                .is_err()
        );
        sink.request_stop();
        assert!(!sink.snapshot().checked_clean());
        let _ = sink.acquired(tickets[0], 124);
        sink.close(tickets[0], Outcome::Success);
        sink.close(tickets[0], Outcome::Success);
        assert!(sink.snapshot().unchecked && !sink.snapshot().checked_clean());
        for _ in 0..EVENTS + 1 {
            let _ = sink.step(Phase::Ledger, Role::Parent);
        }
        for _ in 0..CLEANUP + 1 {
            sink.cleanup(Cleanup {
                resource: 99,
                role: Role::Lock,
                phase: Phase::Unlock,
                action: Action::Unlock,
                outcome: Outcome::Unchecked(None),
            });
        }
        for _ in 0..FAULTS + 2 {
            sink.primary(Fault::new(
                Phase::Ledger,
                Role::Parent,
                Kind::Invariant,
                None,
            ));
        }
        assert!(sink.snapshot().overflow && !sink.snapshot().checked_clean());
    }
    #[test]
    fn ab1_real_private_flock_busy_release_successor_and_fd_baseline() {
        let baseline = fd_count();
        let f = Fixture::new();
        let mut first = Transaction::acquire(&mut f.context(), KernelOps, Sink::new()).unwrap();
        let original = test_meta(&f.root.join(COMPANION));
        let busy = failed(Transaction::acquire(
            &mut f.context(),
            KernelOps,
            Sink::new(),
        ));
        assert_eq!(busy.primary.unwrap().kind, Kind::Busy);
        assert!(first.release().checked_clean());
        drop(first);
        for _ in 0..64 {
            let mut claim = Transaction::acquire(&mut f.context(), KernelOps, Sink::new()).unwrap();
            assert!(claim.release().checked_clean());
        }
        let after = test_meta(&f.root.join(COMPANION));
        assert_eq!(original, after);
        assert_eq!(fd_count(), baseline);
        assert!(f.root.join(COMPANION).exists());
    }
    #[test]
    fn ab1_real_unsafe_companion_types_modes_links_and_replacements_preserved() {
        for kind in [
            "nonempty",
            "symlink",
            "hardlink",
            "directory",
            "fifo",
            "mode",
        ] {
            let f = Fixture::new();
            let path = f.root.join(COMPANION);
            let target = f.root.join("target");
            fs::write(&target, b"").unwrap();
            match kind {
                "nonempty" => fs::write(&path, b"preserve").unwrap(),
                "symlink" => symlink(&target, &path).unwrap(),
                "hardlink" => fs::hard_link(&target, &path).unwrap(),
                "directory" => fs::create_dir(&path).unwrap(),
                "fifo" => nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o600)).unwrap(),
                _ => {
                    fs::write(&path, b"").unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
                }
            }
            let before = test_meta(&path);
            let report = failed(Transaction::acquire(
                &mut f.context(),
                KernelOps,
                Sink::new(),
            ));
            assert!(!report.checked_clean());
            assert_eq!(test_meta(&path), before, "{kind}");
        }
        let f = Fixture::new();
        let mut claim = Transaction::acquire(&mut f.context(), KernelOps, Sink::new()).unwrap();
        let original = f.root.join("retained-lock");
        fs::rename(f.root.join(COMPANION), &original).unwrap();
        fs::write(f.root.join(COMPANION), b"replacement").unwrap();
        let result = claim.transaction.check_lock(
            nix::unistd::getuid().as_raw(),
            Phase::PostLockStat,
            Phase::PostLockNamed,
        );
        assert!(result.is_err());
        claim.transaction.finish(result.err());
        assert!(!claim.transaction.sink.snapshot().checked_clean());
        assert!(original.exists());
        assert_eq!(fs::read(f.root.join(COMPANION)).unwrap(), b"replacement");
    }

    struct BridgeProof {
        root: PathBuf,
        bad_env: std::cell::Cell<bool>,
        bad_process: std::cell::Cell<bool>,
    }
    impl Proof for BridgeProof {
        fn pid(&self) -> i32 {
            43
        }
        fn now(&self) -> std::result::Result<u64, String> {
            Ok(3000)
        }
        fn environment(&self, name: &str) -> Option<std::ffi::OsString> {
            if self.bad_env.get() {
                return Some("wrong".into());
            }
            Some(match name {
                "PROLETARIUS_APP_TOKEN" => "synthetic-token!1".into(),
                "TMPDIR" => self.root.join("tmp").into(),
                "BLE_SOCKET" => self.root.join("data/estk-ble.sock").into(),
                _ => self.root.join("data").into(),
            })
        }
        fn identity(&self, pid: i32) -> std::result::Result<super::super::ProcessSnapshot, String> {
            let (parent, name) = match pid {
                43 => (42, "backend"),
                42 => (41, "app"),
                41 => (40, "guardian"),
                _ => return Err("injected identity rejected".into()),
            };
            Ok(super::super::ProcessSnapshot {
                pid,
                parent,
                start: (100 + pid as u64, u64::from(self.bad_process.get())),
                executable: self.root.join(name),
                exec_identity: [1; 8],
            })
        }
    }
    struct BridgeFixture {
        fixture: Fixture,
        proof: BridgeProof,
        args: Vec<std::ffi::OsString>,
    }
    impl BridgeFixture {
        fn new() -> Self {
            use sha2::{Digest, Sha256};
            let fixture = Fixture::new();
            let root = &fixture.root;
            for name in ["data", "evidence", "tmp"] {
                fs::create_dir(root.join(name)).unwrap();
                fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut parent = serde_json::json!({"schema_version":1,"role":"app","purpose":"switch-no-device-software-acceptance",
                "run_id":"a".repeat(32),"nonce":"b".repeat(64),"guardian_pid":41,"uid":nix::unistd::getuid().as_raw(),
                "created_unix_ms":1000,"expires_unix_ms":100000});
            for (key, name) in [
                ("capsule_root", ""),
                ("data_root", "data"),
                ("evidence_root", "evidence"),
                ("tmp_root", "tmp"),
            ] {
                let path = if name.is_empty() {
                    root.clone()
                } else {
                    root.join(name)
                };
                let m = fs::metadata(&path).unwrap();
                parent[key] = path.to_str().unwrap().into();
                parent[format!("{key}_dev")] = m.dev().into();
                parent[format!("{key}_ino")] = m.ino().into();
            }
            for name in ["app", "backend", "guardian"] {
                fs::write(root.join(name), name).unwrap();
                fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
                parent[format!("{name}_path")] = root.join(name).to_str().unwrap().into();
                parent[format!("{name}_sha256")] =
                    format!("{:x}", Sha256::digest(name.as_bytes())).into();
            }
            let bytes = serde_json::to_vec(&parent).unwrap();
            fs::write(root.join("app.json"), &bytes).unwrap();
            fs::set_permissions(root.join("app.json"), fs::Permissions::from_mode(0o600)).unwrap();
            let mut child = parent;
            child["role"] = "daemon".into();
            child["app_pid"] = 42.into();
            child["created_unix_ms"] = 2000.into();
            child["expires_unix_ms"] = 30000.into();
            child["child_nonce"] = "c".repeat(64).into();
            child["parent_manifest_path"] = root.join("app.json").to_str().unwrap().into();
            child["parent_manifest_inode"] =
                fs::metadata(root.join("app.json")).unwrap().ino().into();
            child["parent_manifest_sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
            let path = root.join(format!("data/daemon-{}.json", "c".repeat(64)));
            fs::write(&path, serde_json::to_vec(&child).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let args = vec![
                "-listen".into(),
                format!("unix:{}/data/switch.sock", root.display()).into(),
                "-backend=at".into(),
                format!("--switch-acceptance-capsule={}", path.display()).into(),
            ];
            let proof = BridgeProof {
                root: root.clone(),
                bad_env: std::cell::Cell::new(false),
                bad_process: std::cell::Cell::new(false),
            };
            Self {
                fixture,
                proof,
                args,
            }
        }
        fn admission(&self) -> AcceptanceAdmission {
            match super::super::classify_and_admit(&self.args, &self.proof, |_| Ok(())).unwrap() {
                super::super::Entry::Acceptance(a) => a,
                _ => panic!("fixture fell back"),
            }
        }
    }
    #[test]
    fn b2_retained_bridge_real_b1_admission_legal_mtime_borrowed_fds_and_fd_baseline() {
        let f = BridgeFixture::new();
        let mut admission = f.admission();
        let baseline = fd_count();
        let held: Vec<_> = admission
            .binding
            .held
            .iter()
            .map(|h| h.file.as_raw_fd())
            .collect();
        fs::write(
            f.fixture.root.join("data/legal-namespace-entry"),
            b"mtime mutation",
        )
        .unwrap();
        for _ in 0..8 {
            validate_retained(&mut admission, &f.proof).unwrap();
            assert_eq!(fd_count(), baseline);
        }
        let mut claim = acquire_for_admission(&mut admission, &f.proof, Sink::new()).unwrap();
        assert_eq!(fd_count(), baseline + 2);
        assert!(claim.release().checked_clean());
        drop(claim);
        assert_eq!(fd_count(), baseline);
        assert_eq!(
            held,
            admission
                .binding
                .held
                .iter()
                .map(|h| h.file.as_raw_fd())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn b2_retained_bridge_replacement_symlink_rename_parent_missing_or_ambiguous_fail_closed() {
        for change in 0..7 {
            let f = BridgeFixture::new();
            let mut a = f.admission();
            let root = &f.fixture.root;
            match change {
                0 => {
                    fs::rename(root.join("backend"), root.join("old-backend")).unwrap();
                    fs::write(root.join("backend"), b"backend").unwrap();
                }
                1 => {
                    fs::rename(root.join("backend"), root.join("old-backend")).unwrap();
                    symlink(root.join("old-backend"), root.join("backend")).unwrap();
                }
                2 => {
                    fs::rename(root.join("data"), root.join("old-data")).unwrap();
                    fs::create_dir(root.join("data")).unwrap();
                    fs::set_permissions(root.join("data"), fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                3 => {
                    fs::rename(root, root.with_extension("retained-capsule")).unwrap();
                    fs::create_dir(root).unwrap();
                    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
                }
                4 => {
                    let data = root.join("data").to_str().unwrap().to_owned();
                    a.binding.held.retain(|h| h.path != data);
                }
                5 => {
                    let file = std::fs::File::open(root.join("tmp")).unwrap();
                    let identity = super::super::Identity::of(&file).unwrap();
                    a.binding.held.push(super::super::Held {
                        path: root.join("data").to_str().unwrap().into(),
                        file,
                        identity,
                        directory: true,
                    });
                }
                _ => {
                    fs::set_permissions(root.join("data"), fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
            }
            assert!(
                validate_retained(&mut a, &f.proof).is_err(),
                "change {change}"
            );
            assert!(a.terminal);
            assert!(validate_retained(&mut a, &f.proof).is_err());
            let view = failed(acquire_for_admission(&mut a, &f.proof, Sink::new()));
            assert!(!view.checked_clean());
            assert!(
                view.resources
                    .iter()
                    .all(|r| r.disposition == Disposition::ReservedUnused)
            );
        }
    }
    #[test]
    fn b2_retained_bridge_process_environment_and_post_lock_drift_are_sticky_terminal() {
        for env in [true, false] {
            let f = BridgeFixture::new();
            let mut a = f.admission();
            if env {
                f.proof.bad_env.set(true);
            } else {
                f.proof.bad_process.set(true);
            }
            assert!(validate_retained(&mut a, &f.proof).is_err());
            f.proof.bad_env.set(false);
            f.proof.bad_process.set(false);
            assert!(validate_retained(&mut a, &f.proof).is_err());
        }
        let f = BridgeFixture::new();
        let mut admission = f.admission();
        let m = &admission.binding.manifest;
        let h = admission
            .binding
            .held
            .iter()
            .find(|h| h.directory && h.path == m.data_root)
            .unwrap();
        let i = &h.identity;
        let anchor = Meta {
            dev: i.dev,
            ino: i.ino,
            uid: i.uid,
            mode: i.mode,
            links: i.links,
            size: i.size,
            mtime: i.mtime,
            ctime: i.ctime,
        };
        let root = PathBuf::from(&m.data_root);
        let sink = Sink::new();
        let (ops, trace) = Injected::new(
            Config {
                drift_file: Some(f.fixture.root.join("backend")),
                ..Config::default()
            },
            sink.clone(),
        );
        let mut context = AdmissionContext {
            admission: &mut admission,
            proof: &f.proof,
            root,
            anchor,
        };
        let report = failed(Transaction::acquire(&mut context, ops, sink));
        assert_eq!(report.primary.unwrap().phase, Phase::ContextAfter);
        assert!(admission.terminal);
        assert_eq!(count(&trace, Call::Unlock), 1);
        assert_eq!(closes(&trace, Role::Lock), 1);
        assert_eq!(closes(&trace, Role::Parent), 1);
        assert!(!report.checked_clean());
    }
    #[test]
    fn r2_all_46_independent_event_counterexamples_reject_checked_clean() {
        let f = Fixture::new();
        let mut claim = Transaction::acquire(&mut f.context(), KernelOps, Sink::new()).unwrap();
        let clean = claim.release();
        assert!(clean.checked_clean());
        let mut count = 0;
        for index in 0..15 {
            let mut view = clean;
            view.events[index] = None;
            assert!(!view.checked_clean());
            count += 1;
            let mut view = clean;
            view.events[index] = clean.events[(index + 1) % 15];
            assert!(!view.checked_clean());
            count += 1;
            let mut view = clean;
            let event = view.events[index].as_mut().unwrap();
            event.role = if event.role == Role::Parent {
                Role::Lock
            } else {
                Role::Parent
            };
            assert!(!view.checked_clean());
            count += 1;
        }
        let mut view = clean;
        view.events[15] = Some(Event {
            phase: Phase::Fallback,
            role: Role::Parent,
        });
        assert!(!view.checked_clean());
        count += 1;
        assert_eq!(count, 46);
        let mut empty = clean;
        empty.events = [None; EVENTS];
        assert!(!empty.checked_clean());
        let mut reversed = clean;
        reversed.events[..15].reverse();
        assert!(!reversed.checked_clean());
        for a in 0..15 {
            for b in a + 1..15 {
                let mut reordered = clean;
                reordered.events.swap(a, b);
                assert!(!reordered.checked_clean());
            }
        }
    }
    #[test]
    fn r2_eight_wrong_capacity_poisons_are_permanent_and_acquire_zero_resources() {
        for dimension in 0..4 {
            for delta in [-1isize, 1] {
                let sink = Sink::new();
                let mut counts = [RESOURCES, CLEANUP, EVENTS, FAULTS];
                counts[dimension] = (counts[dimension] as isize + delta) as usize;
                assert!(
                    sink.reserve(counts[0], counts[1], counts[2], counts[3])
                        .is_err()
                );
                assert!(sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).is_err());
                let f = Fixture::new();
                let (ops, trace) = Injected::new(Config::default(), sink.clone());
                let report = failed(Transaction::acquire(&mut f.context(), ops, sink.clone()));
                assert!(trace.borrow().calls.is_empty());
                assert_eq!(count(&trace, Call::TryLock), 0);
                assert!(report.poisoned && report.unchecked && !report.checked_clean());
                assert_eq!(report.lifecycle, Lifecycle::Sealed);
                assert!(
                    report
                        .resources
                        .iter()
                        .all(|r| r.disposition == Disposition::ReservedUnused)
                );
                assert!(sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).is_err());
            }
        }
        for cause in 0..3 {
            let sink = Sink::new();
            match cause {
                0 => sink.request_stop(),
                1 => sink.fallback(),
                _ => sink.primary(Fault::new(
                    Phase::Reserve,
                    Role::Parent,
                    Kind::Invariant,
                    None,
                )),
            };
            let f = Fixture::new();
            let (ops, trace) = Injected::new(Config::default(), sink.clone());
            failed(Transaction::acquire(&mut f.context(), ops, sink));
            assert!(trace.borrow().calls.is_empty());
        }
    }
    #[test]
    fn r2_sealed_sink_all_late_mutators_only_stick_unchecked_and_do_not_rewrite() {
        for operation in 0..11 {
            let f = Fixture::new();
            let sink = Sink::new();
            let mut claim =
                Transaction::acquire(&mut f.context(), KernelOps, sink.clone()).unwrap();
            let before = claim.release();
            assert!(before.checked_clean());
            let ticket = Ticket {
                index: 0,
                role: Role::Parent,
            };
            match operation {
                0 => {
                    assert!(sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).is_err());
                }
                1 => {
                    assert!(sink.step(Phase::ParentOpen, Role::Parent).is_err());
                }
                2 => {
                    assert!(sink.acquired(ticket, 123).is_err());
                }
                3 => {
                    assert!(sink.lock(LockState::Held).is_err());
                }
                4 => sink.cleanup(Cleanup {
                    resource: 0,
                    role: Role::Parent,
                    phase: Phase::ParentClose,
                    action: Action::Close,
                    outcome: Outcome::Success,
                }),
                5 => sink.close(ticket, Outcome::Success),
                6 => sink.primary(Fault::new(
                    Phase::Ledger,
                    Role::Parent,
                    Kind::Invariant,
                    None,
                )),
                7 => sink.fallback(),
                8 => {
                    assert!(sink.permit_operation().is_err());
                }
                9 => {
                    assert!(sink.authority_ready().is_err());
                }
                _ => {
                    drop(sink.call_guard(Phase::ParentClose, Role::Parent));
                }
            }
            let after = sink.snapshot();
            assert!(after.unchecked && !after.checked_clean());
            let mut original_fields = after;
            original_fields.unchecked = before.unchecked;
            assert_eq!(original_fields, before, "operation {operation}");
            sink.request_stop();
            let same = sink.snapshot();
            assert_eq!(after, same);
            assert_eq!(claim.release(), same);
        }
        let sink = Sink::new();
        let tickets = sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        sink.request_stop();
        let raw_tickets = tickets.tickets;
        tickets.finish.seal();
        let tickets = raw_tickets;
        let sealed = sink.snapshot();
        assert!(sink.acquired(tickets[0], 1).is_err());
        assert!(sink.acquired(tickets[1], 2).is_err());
        assert!(sink.lock(LockState::Held).is_err());
        sink.cleanup(Cleanup {
            resource: 1,
            role: Role::Lock,
            phase: Phase::Unlock,
            action: Action::Unlock,
            outcome: Outcome::Success,
        });
        let _ = sink.lock(LockState::Released);
        sink.close(tickets[1], Outcome::Success);
        sink.close(tickets[0], Outcome::Success);
        sink.request_stop();
        assert!(!sink.snapshot().checked_clean());
        assert_eq!(sink.snapshot(), sealed);
    }
    #[test]
    fn r2_failed_registration_after_each_real_open_aborts_before_next_business_step() {
        for role in [Role::Parent, Role::Lock] {
            for seal in [true, false] {
                let f = Fixture::new();
                let mut context = f.context();
                let baseline = fd_count();
                let sink = Sink::new();
                let config = if seal {
                    Config {
                        seal_after_open: Some(role),
                        ..Config::default()
                    }
                } else {
                    Config {
                        poison_after_open: Some(role),
                        ..Config::default()
                    }
                };
                let (ops, trace) = Injected::new(config, sink.clone());
                let view = failed(Transaction::acquire(&mut context, ops, sink));
                assert!(!view.checked_clean());
                assert_eq!(count(&trace, Call::TryLock), 0);
                assert_eq!(closes(&trace, Role::Parent), 1);
                assert_eq!(closes(&trace, Role::Lock), usize::from(role == Role::Lock));
                let next = if role == Role::Parent {
                    Phase::ParentStat
                } else {
                    Phase::LockStat
                };
                assert!(
                    !trace
                        .borrow()
                        .observations
                        .iter()
                        .any(|(phase, _)| *phase == next)
                );
                assert_eq!(fd_count(), baseline);
            }
        }
    }
    #[test]
    fn r2_callback_terminal_and_flock_poison_block_authority_but_preserve_cleanup() {
        for phase in [Phase::ParentOpen, Phase::Committed] {
            let f = Fixture::new();
            let mut context = f.context();
            let baseline = fd_count();
            let sink = Sink::new();
            let (ops, trace) = Injected::new(
                Config {
                    seal_checkpoint: Some(phase),
                    ..Config::default()
                },
                sink.clone(),
            );
            let report = failed(Transaction::acquire(&mut context, ops, sink));
            assert!(!report.checked_clean());
            assert_eq!(
                count(&trace, Call::Open(Role::Parent)),
                usize::from(phase == Phase::Committed)
            );
            if phase == Phase::Committed {
                assert_eq!(count(&trace, Call::Unlock), 1);
                assert_eq!(closes(&trace, Role::Lock), 1);
                assert_eq!(closes(&trace, Role::Parent), 1);
            }
            assert_eq!(fd_count(), baseline);
        }
        for mask in 0..8 {
            let f = Fixture::new();
            let sink = Sink::new();
            let mut errors = Vec::new();
            if mask & 2 != 0 {
                errors.push((Role::Lock, Errno::EINTR));
            }
            if mask & 4 != 0 {
                errors.push((Role::Parent, Errno::EIO));
            }
            let (ops, trace) = Injected::new(
                Config {
                    poison_after_flock: true,
                    unlock_error: mask & 1 != 0,
                    close_errors: errors,
                    ..Config::default()
                },
                sink.clone(),
            );
            let report = failed(Transaction::acquire(&mut f.context(), ops, sink));
            assert!(!report.checked_clean());
            assert_eq!(count(&trace, Call::Unlock), 1);
            assert_eq!(closes(&trace, Role::Lock), 1);
            assert_eq!(closes(&trace, Role::Parent), 1);
            assert_eq!(report.cleanup.iter().flatten().count(), 3);
            assert_eq!(
                report
                    .cleanup
                    .iter()
                    .flatten()
                    .filter(|c| matches!(c.outcome, Outcome::Error(_)))
                    .count(),
                (mask as u32).count_ones() as usize
            );
            assert!(
                !trace
                    .borrow()
                    .observations
                    .iter()
                    .any(|(p, _)| *p == Phase::PostLock)
            );
        }
    }
    #[test]
    fn r2_success_event_bound_plus_one_and_event_only_fabrication_reject() {
        let f = Fixture::new();
        let sink = Sink::new();
        let mut claim = Transaction::acquire(&mut f.context(), KernelOps, sink.clone()).unwrap();
        assert_eq!(sink.snapshot().events.iter().flatten().count(), 15);
        assert!(sink.step(Phase::Fallback, Role::Parent).is_err());
        let report = claim.release();
        assert!(!report.checked_clean());
        assert_eq!(report.cleanup.iter().flatten().count(), 3);
        let sink = Sink::new();
        sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        for event in SUCCESS_EVENTS {
            sink.step(event.phase, event.role).unwrap();
        }
        assert!(sink.authority_ready().is_err());
        sink.request_stop();
        assert!(!sink.snapshot().checked_clean());
        let sink = Sink::new();
        let tickets = sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        sink.step(Phase::ContextBefore, Role::Parent).unwrap();
        sink.step(Phase::ParentOpen, Role::Parent).unwrap();
        sink.acquired(tickets[0], 123).unwrap();
        sink.close(tickets[0], Outcome::Success);
        assert!(sink.snapshot().poisoned);
        assert!(sink.permit_operation().is_err());
        sink.request_stop();
        assert!(!sink.snapshot().checked_clean());
    }
    #[test]
    fn r3_registration_stop_and_poison_preserve_every_physical_receipt_and_close_errno() {
        for role in [Role::Parent, Role::Lock] {
            for request in [true, false] {
                for mask in 0..4 {
                    let f = Fixture::new();
                    let mut context = f.context();
                    let baseline = fd_count();
                    let sink = Sink::new();
                    let mut errors = Vec::new();
                    if mask & 1 != 0 {
                        errors.push((Role::Parent, Errno::EINTR));
                    }
                    if mask & 2 != 0 {
                        errors.push((Role::Lock, Errno::EIO));
                    }
                    let (ops, trace) = Injected::new(
                        Config {
                            seal_after_open: request.then_some(role),
                            poison_after_open: (!request).then_some(role),
                            close_errors: errors.clone(),
                            ..Config::default()
                        },
                        sink.clone(),
                    );
                    let view = failed(Transaction::acquire(&mut context, ops, sink));
                    assert_eq!(view.lifecycle, Lifecycle::Sealed);
                    assert!(view.terminal && view.primary.is_some() && !view.checked_clean());
                    let physical: Vec<_> = trace
                        .borrow()
                        .calls
                        .iter()
                        .filter_map(|c| {
                            if let Call::Close(role, fd) = c {
                                Some((*role, *fd))
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(physical.len(), if role == Role::Lock { 2 } else { 1 });
                    let cleanup: Vec<_> = view.cleanup.iter().flatten().collect();
                    assert_eq!(cleanup.len(), physical.len());
                    for ((r, fd), receipt) in physical.iter().zip(cleanup) {
                        let index = usize::from(*r == Role::Lock);
                        assert_eq!(view.resources[index].observed_fd, Some(*fd));
                        assert_eq!(receipt.resource, index);
                        assert_eq!(receipt.role, *r);
                        assert_eq!(receipt.action, Action::Close);
                        assert_eq!(
                            receipt.phase,
                            if *r == Role::Lock {
                                Phase::LockClose
                            } else {
                                Phase::ParentClose
                            }
                        );
                        let error = errors.iter().find(|(v, _)| v == r).map(|(_, e)| *e);
                        assert_eq!(
                            receipt.outcome,
                            error.map_or(Outcome::Success, Outcome::Error)
                        );
                        assert_eq!(
                            view.resources[index].disposition,
                            if error.is_some() {
                                Disposition::CloseFailed
                            } else {
                                Disposition::Closed
                            }
                        );
                    }
                    assert_eq!(count(&trace, Call::TryLock), 0);
                    assert_eq!(closes(&trace, Role::Parent), 1);
                    assert_eq!(closes(&trace, Role::Lock), usize::from(role == Role::Lock));
                    let next = if role == Role::Parent {
                        Phase::ParentStat
                    } else {
                        Phase::LockStat
                    };
                    assert!(!trace.borrow().observations.iter().any(|(p, _)| *p == next));
                    assert_eq!(fd_count(), baseline);
                }
            }
        }
    }
    #[test]
    fn r3_ops_borrow_conflict_unwinds_via_same_completion_permit_and_keeps_cleanup() {
        let f = Fixture::new();
        let mut context = f.context();
        let baseline = fd_count();
        let sink = Sink::new();
        let mut claim = Transaction::acquire(&mut context, KernelOps, sink.clone()).unwrap();
        let ops = Rc::clone(&claim.transaction.ops);
        let held = ops.borrow_mut();
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                claim.release();
            }))
            .is_err()
        );
        drop(held);
        drop(claim);
        let view = sink.snapshot();
        assert_eq!(view.lifecycle, Lifecycle::Sealed);
        assert!(view.primary.is_some() && view.unchecked && !view.checked_clean());
        assert_eq!(view.cleanup.iter().flatten().count(), 3);
        assert!(
            view.resources
                .iter()
                .all(|r| r.observed_fd.is_some() && r.disposition == Disposition::UncheckedClosed)
        );
        assert_eq!(fd_count(), baseline);
    }
    #[test]
    fn r3_registration_stop_fd_number_reuse_is_never_closed_again() {
        for role in [Role::Parent, Role::Lock] {
            let f = Fixture::new();
            let mut context = f.context();
            let baseline = fd_count();
            let sink = Sink::new();
            let (ops, trace) = Injected::new(
                Config {
                    seal_after_open: Some(role),
                    reuse: Some(role),
                    reuse_path: f.root.join("replacement-fixture"),
                    close_errors: vec![(role, Errno::EINTR)],
                    ..Config::default()
                },
                sink.clone(),
            );
            let view = failed(Transaction::acquire(&mut context, ops, sink.clone()));
            assert_eq!(closes(&trace, role), 1);
            assert!(
                view.cleanup
                    .iter()
                    .flatten()
                    .any(|c| c.role == role && c.outcome == Outcome::Error(Errno::EINTR))
            );
            let before = view;
            sink.request_stop();
            assert_eq!(sink.snapshot(), before);
            let replacement = trace.borrow_mut().replacement.take().unwrap();
            assert!(fstat(&replacement).is_ok());
            drop(replacement);
            assert_eq!(fd_count(), baseline);
        }
    }
}
