//! Scoped access to the separately built test allocator. No runtime setting
//! can enable this module in the default production codec archive.
use std::{fmt, marker::PhantomData, rc::Rc};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub token: u64,
    pub attempts: u64,
    pub allocations: u64,
    pub frees: u64,
    pub live_blocks: u64,
    pub live_bytes: u64,
    pub allocated_bytes: u64,
    pub injected_failures: u64,
    pub system_failures: u64,
    pub tracking_errors: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultError;

impl fmt::Display for FaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("native codec fault-test scope or token is invalid")
    }
}
impl std::error::Error for FaultError {}

#[allow(unsafe_code)]
unsafe extern "C" {
    fn switch_test_codec_alloc_begin(fail_at: u64) -> u64;
    fn switch_test_codec_alloc_end(token: u64) -> i32;
    fn switch_test_codec_alloc_report(token: u64, report: *mut Report) -> i32;
    fn switch_test_codec_alloc_global(report: *mut Report);
}

/// Must begin and end on the constructor thread. Allocated blocks retain the
/// token after this scope ends, so their eventual cross-thread free is counted.
pub struct FaultScope {
    token: u64,
    active: bool,
    same_thread: PhantomData<Rc<()>>,
}

impl FaultScope {
    #[allow(unsafe_code)]
    pub fn begin(fail_at: u64) -> Result<Self, FaultError> {
        let token = unsafe { switch_test_codec_alloc_begin(fail_at) };
        if token == 0 {
            return Err(FaultError);
        }
        Ok(Self {
            token,
            active: true,
            same_thread: PhantomData,
        })
    }

    pub fn token(&self) -> u64 {
        self.token
    }

    #[allow(unsafe_code)]
    pub fn finish(mut self) -> Result<(), FaultError> {
        let result = unsafe { switch_test_codec_alloc_end(self.token) };
        self.active = false;
        if result == 0 { Ok(()) } else { Err(FaultError) }
    }
}

impl Drop for FaultScope {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if self.active {
            let _ = unsafe { switch_test_codec_alloc_end(self.token) };
        }
    }
}

#[allow(unsafe_code)]
pub fn report(token: u64) -> Result<Report, FaultError> {
    let mut report = Report::default();
    if unsafe { switch_test_codec_alloc_report(token, &mut report) } == 0 {
        Ok(report)
    } else {
        Err(FaultError)
    }
}

#[allow(unsafe_code)]
pub fn global_report() -> Report {
    let mut report = Report::default();
    unsafe { switch_test_codec_alloc_global(&mut report) };
    report
}
