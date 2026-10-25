//! Explicit initial-setup completion only. The caller freshly owns credential,
//! bootstrap and historical writer locks through Keychain add/readback. A
//! diagnostic Observation is never accepted by this API as a recovery grant.
use super::*;
use nix::sys::stat::fchmod;
use std::{ffi::c_void, io::Write, panic::AssertUnwindSafe};

const REFUSED: i32 = -16;
const EXISTS: i32 = -11;

struct Recovery {
    evidence: Observation,
    expected: [u8; 40],
    ready: bool,
    failed: bool,
    #[cfg(test)]
    before_create: Option<Box<dyn FnOnce()>>,
}
impl Recovery {
    fn begin(directory: i32, key: &[u8]) -> Result<Self, i32> {
        if key.len() != 32 {
            return Err(INVALID);
        }
        let (kind, matches, evidence) = observe(directory, key)?;
        if kind & 15 != 3
            || matches.len() != 1
            || matches[0] & 15 != 1
            || !matches!(matches[0] >> 4, 0 | 1)
        {
            return Err(REFUSED);
        }
        let mut expected = [0_u8; 40];
        expected[..8].copy_from_slice(b"SWFKID1\0");
        let mut hash = Sha256::new();
        hash.update(b"Switch.SMSForwarding.QueueKey.v1\0");
        hash.update(key);
        expected[8..].copy_from_slice(&hash.finalize());
        Ok(Self {
            evidence,
            expected,
            ready: false,
            failed: false,
            #[cfg(test)]
            before_create: None,
        })
    }
    fn prepare(&mut self) -> Result<(), i32> {
        if self.failed {
            return Err(REFUSED);
        }
        let result = self.prepare_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn prepare_inner(&mut self) -> Result<(), i32> {
        self.evidence.confirm()?;
        let directory = unsafe { BorrowedFd::borrow_raw(self.evidence.directory.as_raw_fd()) };
        if self.evidence.witness.is_none() {
            // Only genuine absence can be completed. Never replace or repair an
            // existing malformed/mismatched witness, including our partial one.
            #[cfg(test)]
            if let Some(before_create) = self.before_create.take() {
                before_create();
            }
            let descriptor = openat(
                directory,
                "state.enc.keyid",
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|e| if e == Errno::EEXIST { EXISTS } else { IO })?;
            let mut file: File = descriptor.into();
            fchmod(&file, Mode::from_bits_truncate(0o600)).map_err(|_| IO)?;
            let original = file.metadata().map_err(|_| IO)?;
            if !private_file(&original) {
                return Err(UNSAFE);
            }
            if fault("partial") {
                file.write_all(&self.expected[..8]).map_err(|_| IO)?;
                file.sync_all().map_err(|_| IO)?;
                self.evidence.directory.sync_all().map_err(|_| IO)?;
                return Err(IO);
            }
            file.write_all(&self.expected).map_err(|_| IO)?;
            if fault("file-sync") {
                return Err(IO);
            }
            file.sync_all().map_err(|_| IO)?;
            if fault("dir-sync") {
                return Err(IO);
            }
            self.evidence.directory.sync_all().map_err(|_| IO)?;
            let captured = capture(directory, "state.enc.keyid", 40)?.ok_or(UNSAFE)?;
            if captured.metadata.dev() != original.dev()
                || captured.metadata.ino() != original.ino()
                || captured.bytes != self.expected
            {
                return Err(UNSAFE);
            }
            self.evidence.witness = Some(captured);
        }
        // Reconfirm durability even when reopening a complete witness after a
        // previous lost sync/ACK. No queue/witness bytes are rewritten here.
        self.evidence
            .queue
            .as_ref()
            .ok_or(REFUSED)?
            .file
            .sync_all()
            .map_err(|_| IO)?;
        self.evidence
            .witness
            .as_ref()
            .ok_or(REFUSED)?
            .file
            .sync_all()
            .map_err(|_| IO)?;
        self.evidence.directory.sync_all().map_err(|_| IO)?;
        self.evidence.confirm()?;
        self.ready = true;
        Ok(())
    }
    fn confirm(&mut self) -> Result<(), i32> {
        if self.failed || !self.ready {
            return Err(REFUSED);
        }
        let result = self.evidence.confirm();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
#[cfg(feature = "fault-injection")]
fn fault(phase: &str) -> bool {
    std::env::var("SWITCH_FORWARDING_RECOVERY_WITNESS_FAULT").is_ok_and(|s| s == phase)
}
#[cfg(not(feature = "fault-injection"))]
fn fault(_: &str) -> bool {
    false
}

/// # Safety
/// Key contains exactly 32 readable bytes; output is a writable handle slot.
/// Caller owns all three original locks and directory throughout this handle's
/// lifetime, freshly verifies a unique complete stage and all active absence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_recovery_begin(
    directory: i32,
    key: *const u8,
    length: usize,
    output: *mut *mut c_void,
) -> i32 {
    if key.is_null() || length != 32 || output.is_null() {
        return INVALID;
    }
    unsafe {
        *output = std::ptr::null_mut();
    }
    match std::panic::catch_unwind(|| {
        Recovery::begin(directory, unsafe {
            std::slice::from_raw_parts(key, length)
        })
    }) {
        Ok(Ok(proof)) => {
            unsafe {
                *output = Box::into_raw(Box::new(proof)).cast();
            }
            0
        }
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}
/// # Safety
/// Handle is live from recovery_begin, exclusively used on its owning worker.
/// Only an explicitly authorized recovery may call this mutating witness step.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_recovery_prepare_witness(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return INVALID;
    }
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *handle.cast::<Recovery>() }.prepare()
    })) {
        Ok(Ok(())) => 0,
        Ok(Err(code)) => code,
        Err(_) => {
            unsafe { &mut *handle.cast::<Recovery>() }.failed = true;
            PANIC
        }
    }
}
/// # Safety
/// Handle remains exclusively owned and all three locks remain held through
/// actual Keychain add/readback. Confirm immediately before/after that commit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_recovery_confirm(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return INVALID;
    }
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *handle.cast::<Recovery>() }.confirm()
    })) {
        Ok(Ok(())) => 0,
        Ok(Err(code)) => code,
        Err(_) => {
            unsafe { &mut *handle.cast::<Recovery>() }.failed = true;
            PANIC
        }
    }
}
/// # Safety
/// Destroy a live recovery handle once, after its true commit owner retires.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_forwarding_recovery_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Recovery>()) });
    }
}
#[cfg(test)]
mod tests;
