use crate::{Error, Exit, State};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command};
#[cfg(test)]
use std::time::{Duration, Instant};

/// Used exclusively by the single-threaded standalone owner. No other thread
/// or signal handler may call wait/waitpid for this child.
pub(crate) struct ChildOwner {
    child: Option<Child>,
    pid: u32,
    exit: Option<Exit>,
    term_sent: bool,
    wait_failed: bool,
}
pub(crate) fn reset_sigchld() -> Result<(), Error> {
    // SAFETY: sigaction is initialized in full; SIG_DFL plus zero flags clears
    // inherited SIG_IGN/SA_NOCLDWAIT. No handler or borrowed pointer escapes.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        action.sa_flags = 0;
        if libc::sigemptyset(&mut action.sa_mask) != 0
            || libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) != 0
        {
            return Err(Error::Child);
        }
    }
    Ok(())
}
impl ChildOwner {
    pub(crate) fn spawn(mut command: Command) -> Result<Self, Error> {
        reset_sigchld()?;
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: only async-signal-safe syscalls run after fork. The
            // parent identity is checked on both sides of PR_SET_PDEATHSIG;
            // the signal is not retroactive if the parent died first.
            unsafe {
                command.pre_exec(move || {
                    if libc::getppid() != parent {
                        return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                    }
                    let mut mask: libc::sigset_t = std::mem::zeroed();
                    if libc::sigemptyset(&mut mask) != 0
                        || libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut()) != 0
                        || libc::signal(libc::SIGTERM, libc::SIG_DFL) == libc::SIG_ERR
                        || libc::prctl(
                            libc::PR_SET_PDEATHSIG,
                            libc::SIGTERM as libc::c_ulong,
                            0 as libc::c_ulong,
                            0 as libc::c_ulong,
                            0 as libc::c_ulong,
                        ) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        libc::raise(libc::SIGTERM);
                        return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = &mut command;
        }
        let child = command.spawn().map_err(|_| Error::Child)?;
        let pid = child.id();
        Ok(Self {
            child: Some(child),
            pid,
            exit: None,
            term_sent: false,
            wait_failed: false,
        })
    }
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
    pub(crate) fn exit(&self) -> Exit {
        self.exit.unwrap_or(Exit::None)
    }
    pub(crate) fn poll(&mut self) -> Result<State, Error> {
        if self.exit.is_some() {
            return Ok(State::Exited);
        }
        if self.wait_failed {
            return Err(Error::Child);
        }
        let child = self.child.as_mut().ok_or(Error::Child)?;
        match child.try_wait() {
            Ok(Some(status)) => {
                // try_wait has reaped. Commit terminal state before returning;
                // no later path may signal this numeric PID.
                let exit = if let Some(code) = status.code() {
                    Exit::Code(code)
                } else if let Some(signal) = status.signal() {
                    Exit::Signal(signal)
                } else {
                    self.wait_failed = true;
                    return Err(Error::Child);
                };
                self.exit = Some(exit);
                drop(self.child.take());
                Ok(State::Exited)
            }
            Ok(None) => Ok(if self.term_sent {
                State::Stopping
            } else {
                State::Running
            }),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => self.poll(),
            Err(_) => {
                self.wait_failed = true;
                Err(Error::Child)
            }
        }
    }
    pub(crate) fn stop_once(&mut self) -> Result<State, Error> {
        let state = self.poll()?;
        if state == State::Exited || self.term_sent {
            return Ok(state);
        }
        // There is no intervening wait handler/thread. An exited-but-unreaped
        // child remains a zombie whose PID cannot be reused here.
        self.term_sent = true;
        // SAFETY: pid came from our still-unreaped Child; SIGCHLD was reset and
        // only this owner performs wait. No PID from a file/request is used.
        if unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) } != 0 {
            // Never retry a failed signal against a possibly ambiguous PID.
            self.poll()?;
            return Err(Error::Child);
        }
        self.poll()
    }
    #[cfg(test)]
    fn wait_until(&mut self, deadline: Instant) -> Result<bool, Error> {
        while Instant::now() < deadline {
            if self.poll()? == State::Exited {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(self.poll()? == State::Exited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shell(script: &str) -> ChildOwner {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        ChildOwner::spawn(command).unwrap()
    }
    #[test]
    fn owned_normal_exit_is_terminal_before_any_later_stop() {
        let mut owner = shell("exit 7");
        assert!(
            owner
                .wait_until(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
        assert_eq!(owner.exit(), Exit::Code(7));
        assert_eq!(owner.stop_once().unwrap(), State::Exited);
        assert!(!owner.term_sent);
        assert!(owner.child.is_none());
        // A separate live child is never selected by a PID file or old PID.
        let mut foreign = shell("sleep 1");
        owner.stop_once().unwrap();
        assert_eq!(foreign.poll().unwrap(), State::Running);
        foreign.stop_once().unwrap();
        assert!(
            foreign
                .wait_until(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_child_unblocks_inherited_term_before_exec() {
        // Block only this test thread; the new child inherits this mask and
        // production pre_exec must clear it before helper exec.
        struct Restore(libc::sigset_t);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut());
                }
            }
        }
        let restore = unsafe {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            let mut previous: libc::sigset_t = std::mem::zeroed();
            assert_eq!(libc::sigemptyset(&mut blocked), 0);
            assert_eq!(libc::sigaddset(&mut blocked, libc::SIGTERM), 0);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous),
                0
            );
            Restore(previous)
        };
        let mut command = Command::new("/bin/sleep");
        command.arg("3");
        let mut owner = ChildOwner::spawn(command).unwrap();
        drop(restore);
        owner.stop_once().unwrap();
        assert!(
            owner
                .wait_until(Instant::now() + Duration::from_secs(1))
                .unwrap()
        );
        assert_eq!(owner.exit(), Exit::Signal(libc::SIGTERM));
    }
    #[test]
    fn timeout_and_repeated_stop_retain_the_same_owned_child() {
        use std::io::Read;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "trap '' TERM; printf R; sleep 0.35; exit 0"])
            .stdout(std::process::Stdio::piped());
        let mut owner = ChildOwner::spawn(command).unwrap();
        // The real child writes only after installing its TERM disposition.
        // This proves fixture readiness, not ARM/helper/PDEATHSIG acceptance.
        let mut ready = owner.child.as_mut().unwrap().stdout.take().unwrap();
        let mut marker = [0u8; 1];
        ready.read_exact(&mut marker).unwrap();
        assert_eq!(&marker, b"R");
        drop(ready);
        owner.stop_once().unwrap();
        assert!(owner.term_sent);
        assert!(
            !owner
                .wait_until(Instant::now() + Duration::from_millis(15))
                .unwrap()
        );
        assert_eq!(owner.stop_once().unwrap(), State::Stopping);
        assert!(owner.child.is_some());
        assert!(
            owner
                .wait_until(Instant::now() + Duration::from_secs(2))
                .unwrap()
        );
    }
}
