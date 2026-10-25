//! Connection retry policy. CoreBluetooth, physical receipts and timers remain
//! in the adapter; it feeds events into this single-owner pure state machine.
use std::ffi::c_void;

#[derive(Default)]
struct Reconnect {
    epoch: u64,
    level: u8,
    stopped: bool,
    next_ticket: u64,
    pending: Option<u64>,
}

impl Reconnect {
    fn event(&mut self, epoch: u64, event: u8) -> u64 {
        if event == 1 {
            // Refuse to recycle identities even after counter exhaustion.
            let Some(next) = self.epoch.checked_add(1) else {
                self.stopped = true;
                self.pending = None;
                return 0;
            };
            self.epoch = next;
            self.level = 0;
            self.pending = None;
            self.stopped = false;
            return next;
        }
        if epoch != self.epoch {
            return 0;
        }
        match event {
            2 | 3 => {
                self.stopped = true;
                self.pending = None;
                // Explicit rejection retains the existing adapter's action:
                // forget this selected target; never try another credential.
                u64::from(event == 3)
            }
            4 => {
                self.level = 0;
                self.pending = None;
                0
            }
            5 => {
                self.pending = None;
                0
            }
            _ => 0,
        }
    }

    fn schedule(&mut self, epoch: u64) -> u64 {
        if epoch != self.epoch || epoch == 0 || self.stopped || self.pending.is_some() {
            return 0;
        }
        let Some(ticket) = self.next_ticket.checked_add(1) else {
            self.stopped = true;
            return 0;
        };
        self.next_ticket = ticket;
        self.level = self.level.saturating_add(1).min(5);
        self.pending = Some(ticket);
        ticket
    }

    fn delay(&self, ticket: u64) -> u32 {
        if self.pending != Some(ticket) || self.level == 0 {
            return 0;
        }
        // Golden Swift behavior: 1,2,4,8,16,16,... indefinitely. Five caps
        // the backoff level; it was never a maximum retry count or 30s delay.
        1 << (self.level - 1)
    }

    fn fire(&mut self, epoch: u64, ticket: u64) -> bool {
        if epoch != self.epoch || self.stopped || self.pending != Some(ticket) {
            return false;
        }
        self.pending = None;
        true
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_reconnect_create() -> *mut c_void {
    Box::into_raw(Box::<Reconnect>::default()).cast()
}

/// # Safety
/// `handle` is null or a live create-owned handle, destroyed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_reconnect_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Reconnect>()) });
    }
}

/// Events: 1=new explicit selection, 2=manual stop, 3=authentication rejection,
/// 4=ready, 5=cancel platform timer. New selection returns epoch; rejection returns forget-target action.
/// # Safety
/// A non-null handle is live and exclusively owned for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_reconnect_event(
    handle: *mut c_void,
    epoch: u64,
    event: u8,
) -> u64 {
    unsafe { handle.cast::<Reconnect>().as_mut() }.map_or(0, |state| state.event(epoch, event))
}

/// # Safety
/// A non-null handle is live and exclusively owned for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_reconnect_schedule(handle: *mut c_void, epoch: u64) -> u64 {
    unsafe { handle.cast::<Reconnect>().as_mut() }.map_or(0, |state| state.schedule(epoch))
}

/// # Safety
/// A non-null handle is live and has no concurrent mutable access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_reconnect_delay(handle: *const c_void, ticket: u64) -> u32 {
    unsafe { handle.cast::<Reconnect>().as_ref() }.map_or(0, |state| state.delay(ticket))
}

/// # Safety
/// A non-null handle is live and exclusively owned for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_reconnect_fire(
    handle: *mut c_void,
    epoch: u64,
    ticket: u64,
) -> u8 {
    unsafe { handle.cast::<Reconnect>().as_mut() }
        .map_or(0, |state| u8::from(state.fire(epoch, ticket)))
}

/// # Safety
/// A non-null handle is live and has no concurrent mutable access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_reconnect_stopped(handle: *const c_void) -> u8 {
    unsafe { handle.cast::<Reconnect>().as_ref() }.map_or(1, |state| u8::from(state.stopped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_delays_cap_level_without_limiting_attempts() {
        let mut state = Reconnect::default();
        let epoch = state.event(0, 1);
        for expected in [1, 2, 4, 8, 16, 16, 16] {
            let ticket = state.schedule(epoch);
            assert_ne!(ticket, 0);
            assert_eq!(state.delay(ticket), expected);
            assert_eq!(state.schedule(epoch), 0); // duplicate event, same timer
            assert!(state.fire(epoch, ticket));
            assert!(!state.fire(epoch, ticket));
        }
        state.event(epoch, 4);
        let ticket = state.schedule(epoch);
        assert_eq!(state.delay(ticket), 1);
    }

    #[test]
    fn rejection_and_manual_stop_fence_timers_until_explicit_selection() {
        for event in [2, 3] {
            let mut state = Reconnect::default();
            let epoch = state.event(0, 1);
            let ticket = state.schedule(epoch);
            assert_eq!(state.event(epoch, event), u64::from(event == 3));
            assert_eq!(state.schedule(epoch), 0);
            assert!(!state.fire(epoch, ticket));
            let replacement = state.event(epoch, 1);
            let current = state.schedule(replacement);
            assert_eq!(state.event(epoch, 3), 0);
            assert!(!state.fire(epoch, ticket));
            assert!(state.fire(replacement, current));
        }
    }

    #[test]
    fn stale_ticket_cannot_consume_new_timer_in_same_epoch() {
        let mut state = Reconnect::default();
        let epoch = state.event(0, 1);
        let old = state.schedule(epoch);
        assert!(state.fire(epoch, old));
        let new = state.schedule(epoch);
        assert!(!state.fire(epoch, old));
        assert_eq!(state.delay(new), 2);
        assert!(state.fire(epoch, new));
    }

    #[test]
    fn cancelled_timer_and_stale_ready_cannot_reset_current_retry() {
        let mut state = Reconnect::default();
        let old_epoch = state.event(0, 1);
        let old = state.schedule(old_epoch);
        state.event(old_epoch, 5);
        let replacement = state.schedule(old_epoch);
        assert!(!state.fire(old_epoch, old));
        assert!(state.fire(old_epoch, replacement));
        let epoch = state.event(old_epoch, 1);
        let current = state.schedule(epoch);
        state.event(old_epoch, 4);
        assert_eq!(state.delay(current), 1);
        assert!(state.fire(epoch, current));
    }

    #[test]
    fn ffi_null_and_identity_exhaustion_fail_closed() {
        unsafe {
            assert_eq!(switch_ble_reconnect_schedule(std::ptr::null_mut(), 1), 0);
            assert_eq!(switch_ble_reconnect_fire(std::ptr::null_mut(), 1, 1), 0);
            assert_eq!(switch_ble_reconnect_stopped(std::ptr::null()), 1);
            switch_ble_reconnect_destroy(std::ptr::null_mut());
        }
        let mut state = Reconnect {
            epoch: u64::MAX,
            ..Default::default()
        };
        assert_eq!(state.event(0, 1), 0);
        assert_eq!(state.schedule(u64::MAX), 0);
    }
}
