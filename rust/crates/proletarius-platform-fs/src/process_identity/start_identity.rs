//! Additive observation API. No discovery, signalling, or PID-only fallback.
//! A proof is a sampled kernel observation, not immutable process custody.
use std::{
    io,
    path::{Path, PathBuf},
};

/// Constructed only after consistent kernel samples. The full opaque Mach
/// audit identity is retained, including its exec-sensitive PID version;
/// start time alone does not detect same-image exec. No token layout is decoded.
/// Unavailable task-name/audit-token support fails closed.
#[derive(Debug, PartialEq, Eq)]
pub struct ProcessStartIdentity {
    pid: i32,
    parent_pid: i32,
    start_seconds: u64,
    start_microseconds: u64,
    executable: PathBuf,
    exec_identity: [u32; 8],
}
impl ProcessStartIdentity {
    pub fn pid(&self) -> i32 {
        self.pid
    }
    pub fn parent_pid(&self) -> i32 {
        self.parent_pid
    }
    pub fn start_seconds(&self) -> u64 {
        self.start_seconds
    }
    pub fn start_microseconds(&self) -> u64 {
        self.start_microseconds
    }
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn exec_identity(&self) -> &[u32; 8] {
        &self.exec_identity
    }
    pub fn revalidate(&self) -> io::Result<()> {
        if *self == process_start_identity(self.pid)? {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

fn consistent(
    pid: i32,
    mut sample: impl FnMut(i32) -> io::Result<ProcessStartIdentity>,
) -> io::Result<ProcessStartIdentity> {
    if pid <= 1 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let first = sample(pid)?;
    let second = sample(pid)?;
    for proof in [&first, &second] {
        if proof.pid != pid
            || proof.parent_pid <= 0
            || proof.parent_pid == pid
            || proof.start_seconds == 0
            || proof.start_microseconds >= 1_000_000
            || !proof.executable.is_absolute()
            || proof.exec_identity == [0; 8]
        {
            return Err(invalid());
        }
    }
    if first != second {
        return Err(invalid());
    }
    Ok(second)
}

/// Observe one explicit PID twice, with BSD identity bracketing every bound
/// audit-token path read. No retries on churn, short result, exit or denial.
#[cfg(target_os = "macos")]
pub fn process_start_identity(pid: i32) -> io::Result<ProcessStartIdentity> {
    consistent(pid, kernel::sample)
}
#[cfg(not(target_os = "macos"))]
pub fn process_start_identity(_pid: i32) -> io::Result<ProcessStartIdentity> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(target_os = "macos")]
mod kernel {
    use super::*;
    use nix::libc;
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    // ABI declarations from this target's mach/mach_traps.h, task_info.h,
    // mach/message.h and libproc.h. No task-control port is used.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AuditToken {
        words: [u32; 8],
    }
    unsafe extern "C" {
        fn task_name_for_pid(task: u32, pid: i32, name: *mut u32) -> i32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
        fn proc_pidpath_audittoken(
            token: *mut AuditToken,
            buffer: *mut libc::c_void,
            size: u32,
        ) -> i32;
    }
    fn bsd(pid: i32) -> io::Result<libc::proc_bsdinfo> {
        bsd_read(pid, |output, size| {
            // SAFETY: bsd_read supplies an allocated exact-size ABI buffer.
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, output, size) }
        })
    }
    fn bsd_read(
        pid: i32,
        read: impl FnOnce(*mut libc::c_void, i32) -> i32,
    ) -> io::Result<libc::proc_bsdinfo> {
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let count = read(info.as_mut_ptr().cast(), size as i32);
        if count != size as i32 {
            return Err(invalid());
        }
        // SAFETY: kernel returned exactly a full initialized ABI structure.
        let info = unsafe { info.assume_init() };
        // SIDL=1, SZOMB=5 in sys/proc.h; neither is a live admitted process.
        if info.pbi_pid != pid as u32
            || !matches!(info.pbi_status, 2..=4)
            || info.pbi_ppid == 0
            || info.pbi_start_tvsec == 0
            || info.pbi_start_tvusec >= 1_000_000
        {
            return Err(invalid());
        }
        Ok(info)
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn short_or_invalid_kernel_bsd_results_never_form_a_start_proof() {
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
            for count in [-1, 0, 1, size - 1, size + 1, size] {
                // An exact-sized but unpopulated result is also invalid. Short
                // outputs are rejected before assuming any initialized fields.
                assert!(bsd_read(42, |_, _| count).is_err());
            }
        }
    }
    fn key(info: &libc::proc_bsdinfo) -> (u32, u32, u64, u64) {
        (
            info.pbi_pid,
            info.pbi_ppid,
            info.pbi_start_tvsec,
            info.pbi_start_tvusec,
        )
    }
    fn token(pid: i32) -> io::Result<AuditToken> {
        let mut name = 0;
        // SAFETY: local initialized output; this borrows a task-name right only.
        #[allow(deprecated)]
        let task = unsafe { libc::mach_task_self() };
        // SAFETY: task is current task's live port; name is valid writable output.
        let named = unsafe { task_name_for_pid(task, pid, &mut name) };
        if named != 0 || name == 0 {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let mut token = AuditToken { words: [0; 8] };
        let mut count = 8;
        // SAFETY: TASK_AUDIT_TOKEN=15 expects exactly eight integer words.
        let result =
            unsafe { libc::task_info(name, 15, token.words.as_mut_ptr().cast(), &mut count) };
        // SAFETY: consume the one acquired send right once, even on info failure.
        let closed = unsafe { mach_port_deallocate(task, name) };
        if result != 0 || count != 8 || closed != 0 {
            return Err(invalid());
        }
        if token.words == [0; 8] {
            return Err(invalid());
        }
        Ok(token)
    }
    pub(super) fn sample(pid: i32) -> io::Result<ProcessStartIdentity> {
        let before = bsd(pid)?;
        let mut token = token(pid)?;
        let mut bytes = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: token is complete; output allocation is exactly supplied size.
        let count = unsafe {
            proc_pidpath_audittoken(&mut token, bytes.as_mut_ptr().cast(), bytes.len() as u32)
        };
        // libproc returns the path byte count excluding its NUL terminator.
        if count <= 0
            || count as usize >= bytes.len()
            || bytes[count as usize] != 0
            || bytes[..count as usize].contains(&0)
        {
            return Err(invalid());
        }
        bytes.truncate(count as usize);
        let after = bsd(pid)?;
        if key(&before) != key(&after) {
            return Err(invalid());
        }
        Ok(ProcessStartIdentity {
            pid,
            parent_pid: after.pbi_ppid as i32,
            start_seconds: after.pbi_start_tvsec,
            start_microseconds: after.pbi_start_tvusec,
            executable: OsString::from_vec(bytes).into(),
            exec_identity: token.words,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn proof() -> ProcessStartIdentity {
        ProcessStartIdentity {
            pid: 42,
            parent_pid: 41,
            start_seconds: 123,
            start_microseconds: 456,
            executable: "/private/fixture/image".into(),
            exec_identity: [7; 8],
        }
    }
    #[test]
    fn consistent_start_proof_rejects_every_identity_drift_and_errors() {
        assert!(consistent(42, |_| Ok(proof())).is_ok());
        for field in 0..6 {
            let mut calls = 0;
            assert!(
                consistent(42, |_| {
                    calls += 1;
                    let mut p = proof();
                    if calls == 2 {
                        match field {
                            0 => p.pid += 1,
                            1 => p.parent_pid += 1,
                            2 => p.start_seconds += 1,
                            3 => p.start_microseconds += 1,
                            4 => p.executable = "/other".into(),
                            _ => p.exec_identity[0] += 1,
                        }
                    }
                    Ok(p)
                })
                .is_err(),
                "field {field}"
            );
        }
        for pid in [-1, 0, 1] {
            assert!(consistent(pid, |_| panic!("invalid pid sampled")).is_err());
        }
        for field in 0..5 {
            assert!(
                consistent(42, |_| {
                    let mut p = proof();
                    match field {
                        0 => p.parent_pid = 0,
                        1 => p.start_seconds = 0,
                        2 => p.start_microseconds = 1_000_000,
                        3 => p.executable = "relative".into(),
                        _ => p.exec_identity = [0; 8],
                    };
                    Ok(p)
                })
                .is_err()
            );
        }
        for fail_at in [1, 2] {
            let mut calls = 0;
            assert!(
                consistent(42, |_| {
                    calls += 1;
                    if calls == fail_at {
                        Err(invalid())
                    } else {
                        Ok(proof())
                    }
                })
                .is_err()
            );
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_self_start_identity_is_live_and_revalidates() {
        let p = process_start_identity(std::process::id() as i32).unwrap();
        assert_eq!(p.pid(), std::process::id() as i32);
        assert_eq!(p.parent_pid(), nix::unistd::getppid().as_raw());
        assert_eq!(p.executable(), std::env::current_exe().unwrap());
        p.revalidate().unwrap();
        for pid in [-1, 0, 1, i32::MAX] {
            assert!(process_start_identity(pid).is_err());
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_child_fixture_exits_by_pipe_without_signal_and_old_proof_rejects() {
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Command, Stdio},
        };
        // Re-exec only this test binary. The child waits on stdin, never sleeps,
        // discovers processes or receives signals. Product entry is not present.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_identity::start_identity::tests::macos_pipe_child",
                "--nocapture",
            ])
            .env("G8_START_PROOF_PIPE_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.trim() == "G8_PIPE_CHILD_READY" {
                break;
            }
        }
        let observed = process_start_identity(child.id() as i32);
        let revalidated = observed.as_ref().is_ok_and(|p| p.revalidate().is_ok());
        child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
        let status = child.wait().unwrap();
        // Reap the explicit fixture before asserting the live observations.
        assert!(status.success());
        let p = observed.unwrap();
        assert_eq!(p.parent_pid(), std::process::id() as i32);
        assert_eq!(p.executable(), std::env::current_exe().unwrap());
        assert!(revalidated);
        assert!(p.revalidate().is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_pipe_child() {
        if std::env::var_os("G8_START_PROOF_PIPE_CHILD").is_none() {
            return;
        }
        use std::io::{BufRead, Write};
        println!("G8_PIPE_CHILD_READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).unwrap();
        if line == "exec\n" {
            use std::os::unix::process::CommandExt;
            let error = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_identity::start_identity::tests::macos_pipe_child",
                    "--nocapture",
                ])
                .env("G8_START_PROOF_PIPE_CHILD", "2")
                .exec();
            panic!("fixture self-exec failed: {error}");
        }
        assert_eq!(line, "exit\n");
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_same_image_exec_changes_generation_and_rejects_old_proof() {
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Command, Stdio},
        };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_identity::start_identity::tests::macos_pipe_child",
                "--nocapture",
            ])
            .env("G8_START_PROOF_PIPE_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = || {
            let mut line = String::new();
            loop {
                line.clear();
                assert!(output.read_line(&mut line).unwrap() > 0);
                if line.trim() == "G8_PIPE_CHILD_READY" {
                    break;
                }
            }
        };
        ready();
        let before = process_start_identity(child.id() as i32);
        child.stdin.as_mut().unwrap().write_all(b"exec\n").unwrap();
        ready();
        let after = process_start_identity(child.id() as i32);
        let rejected = before.as_ref().is_ok_and(|p| p.revalidate().is_err());
        let current_ok = after.as_ref().is_ok_and(|p| p.revalidate().is_ok());
        child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success());
        let before = before.unwrap();
        let after = after.unwrap();
        assert_eq!(before.pid(), after.pid());
        assert_eq!(before.executable(), after.executable());
        assert_ne!(before.exec_identity(), after.exec_identity());
        assert!(rejected && current_ok);
    }
}
