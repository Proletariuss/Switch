//! Rust-owned remembered-reader proposal. UserDefaults remains a macOS byte
//! store; deciding whether a fresh advertisement may become the target lives
//! here and is bound to a scan generation.
use std::ffi::c_void;

#[derive(Default)]
struct Selection {
    generation: u64,
    remembered: Option<Vec<u8>>,
    selected: Option<Vec<u8>>,
}

fn normalized(value: &[u8]) -> Option<Vec<u8>> {
    if value.len() != 36
        || !value.iter().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(value.iter().map(u8::to_ascii_lowercase).collect())
}

impl Selection {
    fn begin(&mut self, remembered: &[u8]) -> u64 {
        let Some(next) = self.generation.checked_add(1) else {
            self.remembered = None;
            self.selected = None;
            return 0;
        };
        self.generation = next;
        self.remembered = normalized(remembered);
        self.selected = None;
        next
    }

    fn observe(&mut self, generation: u64, candidate: &[u8]) -> bool {
        if generation != self.generation || self.selected.is_some() {
            return false;
        }
        let Some(candidate) = normalized(candidate) else {
            return false;
        };
        if self.remembered.as_ref() != Some(&candidate) {
            return false;
        }
        self.selected = Some(candidate);
        true
    }

    fn explicit(&mut self, candidate: &[u8]) -> bool {
        let Some(candidate) = normalized(candidate) else {
            return false;
        };
        self.selected = Some(candidate);
        true
    }

    fn may_persist(&self, candidate: &[u8]) -> bool {
        normalized(candidate)
            .as_ref()
            .is_some_and(|v| self.selected.as_ref() == Some(v))
    }

    fn stop(&mut self) {
        self.selected = None;
        self.remembered = None;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_target_create() -> *mut c_void {
    Box::into_raw(Box::<Selection>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_target_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Selection>()) });
    }
}

fn bytes<'a>(pointer: *const u8, length: usize) -> Option<&'a [u8]> {
    super::input_slice(pointer, length)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_target_begin(
    handle: *mut c_void,
    remembered: *const u8,
    length: usize,
) -> u64 {
    let Some(state) = (unsafe { handle.cast::<Selection>().as_mut() }) else {
        return 0;
    };
    let Some(value) = bytes(remembered, length) else {
        return 0;
    };
    state.begin(value)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_target_observe(
    handle: *mut c_void,
    generation: u64,
    candidate: *const u8,
    length: usize,
) -> u8 {
    let Some(state) = (unsafe { handle.cast::<Selection>().as_mut() }) else {
        return 0;
    };
    let Some(value) = bytes(candidate, length) else {
        return 0;
    };
    u8::from(state.observe(generation, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_target_explicit(
    handle: *mut c_void,
    candidate: *const u8,
    length: usize,
) -> u8 {
    let Some(state) = (unsafe { handle.cast::<Selection>().as_mut() }) else {
        return 0;
    };
    let Some(value) = bytes(candidate, length) else {
        return 0;
    };
    u8::from(state.explicit(value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_target_may_persist(
    handle: *const c_void,
    candidate: *const u8,
    length: usize,
) -> u8 {
    let Some(state) = (unsafe { handle.cast::<Selection>().as_ref() }) else {
        return 0;
    };
    let Some(value) = bytes(candidate, length) else {
        return 0;
    };
    u8::from(state.may_persist(value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_target_stop(handle: *mut c_void) {
    if let Some(state) = unsafe { handle.cast::<Selection>().as_mut() } {
        state.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &[u8] = b"73B7446E-E6CE-C90C-C844-485389FE8C1D";
    const B: &[u8] = b"11111111-2222-3333-4444-555555555555";

    #[test]
    fn remembered_target_requires_fresh_matching_advertisement() {
        let mut state = Selection::default();
        let generation = state.begin(A);
        assert!(!state.observe(generation, B));
        assert!(state.observe(generation, A));
        assert!(!state.observe(generation, A));
        assert!(state.may_persist(A));
    }

    #[test]
    fn stale_scan_and_invalid_preference_never_propose() {
        let mut state = Selection::default();
        let old = state.begin(A);
        let current = state.begin(b"not-a-uuid");
        assert!(!state.observe(old, A));
        assert!(!state.observe(current, A));
        assert!(state.explicit(B));
        assert!(state.may_persist(B));
        state.stop();
        assert!(!state.may_persist(B));
    }
}
