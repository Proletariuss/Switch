//! Read-only kernel process identity. No process discovery or signalling.
use std::{io, path::PathBuf};
#[cfg(target_os = "macos")]
pub fn process_identity(pid: i32) -> io::Result<(i32, PathBuf)> {
    use nix::libc;
    use std::os::unix::ffi::OsStringExt;
    if pid <= 1 {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: output points to an allocated ABI struct of exactly size bytes.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if count != size as i32 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel reported a complete initialized proc_bsdinfo.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut bytes = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: bytes is writable for its entire supplied size.
    let count = unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    let length = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or(io::ErrorKind::InvalidData)?;
    bytes.truncate(length);
    Ok((
        info.pbi_ppid as i32,
        std::ffi::OsString::from_vec(bytes).into(),
    ))
}
#[cfg(not(target_os = "macos"))]
pub fn process_identity(_pid: i32) -> io::Result<(i32, PathBuf)> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

mod start_identity;
pub use start_identity::{ProcessStartIdentity, process_start_identity};
