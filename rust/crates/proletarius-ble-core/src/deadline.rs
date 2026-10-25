//! Rust-owned BLE phase deadlines. The platform adapter schedules a timer for
//! the returned duration, but only this owner can accept a fired ticket and
//! decide the resulting action.
use std::ffi::c_void;

const MAX_TARGET_BYTES: usize = 128;

#[derive(Default)]
struct Deadline {
    next_ticket: u64,
    pending: Option<Pending>,
}

#[derive(Clone, PartialEq, Eq)]
struct Pending {
    ticket: u64,
    phase: u8,
    target: Vec<u8>,
}

fn phase_duration_ms(phase: u8) -> Option<u64> {
    match phase {
        1 => Some(20_000),     // discovery window
        2 => Some(10_000),     // central initialization
        3..=6 => Some(30_000), // connect/service/characteristic/notify
        7 => Some(120_000),    // physical APDU response
        8 => Some(1_000),      // permission prompt observation
        _ => None,
    }
}

fn phase_action(phase: u8) -> u8 {
    match phase {
        1 => 1,     // stop discovery
        2 => 2,     // fail initialization
        3..=6 => 3, // fail current connection phase and request retry
        7 => 4,     // APDU transport observation expired
        8 => 5,     // observe permission again
        _ => 0,
    }
}

impl Deadline {
    fn arm(&mut self, phase: u8, target: &[u8]) -> u64 {
        if phase_duration_ms(phase).is_none() || target.len() > MAX_TARGET_BYTES {
            self.pending = None;
            return 0;
        }
        let Some(ticket) = self.next_ticket.checked_add(1) else {
            self.pending = None;
            return 0;
        };
        self.next_ticket = ticket;
        self.pending = Some(Pending {
            ticket,
            phase,
            target: target.to_vec(),
        });
        ticket
    }

    fn duration(&self, ticket: u64) -> u64 {
        self.pending
            .as_ref()
            .filter(|p| p.ticket == ticket)
            .and_then(|p| phase_duration_ms(p.phase))
            .unwrap_or(0)
    }

    fn fire(&mut self, ticket: u64, phase: u8, target: &[u8]) -> u8 {
        let expected = Pending {
            ticket,
            phase,
            target: target.to_vec(),
        };
        if self.pending.as_ref() != Some(&expected) {
            return 0;
        }
        self.pending = None;
        phase_action(phase)
    }

    fn cancel(&mut self) {
        self.pending = None;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_deadline_create() -> *mut c_void {
    Box::into_raw(Box::<Deadline>::default()).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_deadline_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Deadline>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_deadline_arm(
    handle: *mut c_void,
    phase: u8,
    target: *const u8,
    target_len: usize,
) -> u64 {
    let Some(state) = (unsafe { handle.cast::<Deadline>().as_mut() }) else {
        return 0;
    };
    let Some(target) = super::input_slice(target, target_len) else {
        return 0;
    };
    state.arm(phase, target)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_deadline_duration_ms(
    handle: *const c_void,
    ticket: u64,
) -> u64 {
    unsafe { handle.cast::<Deadline>().as_ref() }.map_or(0, |state| state.duration(ticket))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_deadline_fire(
    handle: *mut c_void,
    ticket: u64,
    phase: u8,
    target: *const u8,
    target_len: usize,
) -> u8 {
    let Some(state) = (unsafe { handle.cast::<Deadline>().as_mut() }) else {
        return 0;
    };
    let Some(target) = super::input_slice(target, target_len) else {
        return 0;
    };
    state.fire(ticket, phase, target)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_deadline_cancel(handle: *mut c_void) {
    if let Some(state) = unsafe { handle.cast::<Deadline>().as_mut() } {
        state.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_duration_and_action_are_owned_here() {
        let mut owner = Deadline::default();
        for (phase, duration, action) in [
            (1, 20_000, 1),
            (2, 10_000, 2),
            (3, 30_000, 3),
            (6, 30_000, 3),
            (7, 120_000, 4),
            (8, 1_000, 5),
        ] {
            let ticket = owner.arm(phase, b"reader-a");
            assert_eq!(owner.duration(ticket), duration);
            assert_eq!(owner.fire(ticket, phase, b"reader-a"), action);
            assert_eq!(owner.fire(ticket, phase, b"reader-a"), 0);
        }
    }

    #[test]
    fn stale_ticket_phase_and_target_fail_closed() {
        let mut owner = Deadline::default();
        let old = owner.arm(3, b"reader-a");
        let current = owner.arm(4, b"reader-b");
        assert_eq!(owner.fire(old, 3, b"reader-a"), 0);
        assert_eq!(owner.fire(current, 3, b"reader-b"), 0);
        assert_eq!(owner.fire(current, 4, b"reader-a"), 0);
        assert_eq!(owner.fire(current, 4, b"reader-b"), 3);
    }

    #[test]
    fn cancellation_and_invalid_input_are_terminal_for_ticket_only() {
        let mut owner = Deadline::default();
        let ticket = owner.arm(7, b"reader-a");
        owner.cancel();
        assert_eq!(owner.fire(ticket, 7, b"reader-a"), 0);
        assert_eq!(owner.arm(0, b""), 0);
        assert_eq!(owner.arm(3, &[0; MAX_TARGET_BYTES + 1]), 0);
    }
}
