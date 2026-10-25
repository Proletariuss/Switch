//! Unique FD custody with consume-once explicit close. No numeric-fd retry.
use super::reports::{Outcome, Phase, Role, Sink, Ticket};
use nix::errno::Errno;
use std::{
    cell::RefCell,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    rc::Rc,
};

pub(super) trait CloseOps {
    fn close(&mut self, fd: OwnedFd, role: Role) -> Result<(), Errno>;
}
pub(super) struct CheckedFd<O: CloseOps> {
    fd: Option<OwnedFd>,
    ticket: Ticket,
    sink: Sink,
    ops: Rc<RefCell<O>>,
}
impl<O: CloseOps> CheckedFd<O> {
    /// No fallible registration occurs until Transaction stores this owner.
    pub(super) fn adopt_pending(
        fd: OwnedFd,
        ticket: Ticket,
        sink: Sink,
        ops: Rc<RefCell<O>>,
    ) -> Self {
        Self {
            fd: Some(fd),
            ticket,
            sink,
            ops,
        }
    }
    pub(super) fn fd(&self) -> Option<BorrowedFd<'_>> {
        self.fd.as_ref().map(AsFd::as_fd)
    }
    pub(super) fn close_once(&mut self) {
        if let Some(fd) = self.fd.take() {
            // Take before calling a fallible provider. EINTR/errors consume
            // this ownership; a reused descriptor number is never retried.
            let mut guard = self.sink.call_guard(
                match self.ticket.role {
                    Role::Lock => Phase::LockClose,
                    Role::Parent => Phase::ParentClose,
                },
                self.ticket.role,
            );
            let outcome = match self.ops.borrow_mut().close(fd, self.ticket.role) {
                Ok(()) => Outcome::Success,
                Err(errno) => Outcome::Error(errno),
            };
            guard.complete();
            self.sink.close(self.ticket, outcome);
        }
    }
    pub(super) fn fallback_close(&mut self) {
        if let Some(fd) = self.fd.take() {
            self.sink.fallback();
            let error = nix::unistd::close(fd).err();
            self.sink.close(self.ticket, Outcome::Unchecked(error));
        }
    }
}
impl<O: CloseOps> Drop for CheckedFd<O> {
    fn drop(&mut self) {
        self.fallback_close();
    }
}
