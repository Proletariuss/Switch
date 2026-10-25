//! Ordered characteristic-selection policy, preserving the Swift discovery cut.
//! The host supplies CoreBluetooth UUID-equality facts (including its Bluetooth
//! base-UUID aliases) and opaque object IDs; Rust never owns a CB object.
use std::ffi::c_void;

// Normalized observation bits, independent of platform OptionSet raw values.
const WRITE: u8 = 1;
const WRITE_WITHOUT_RESPONSE: u8 = 2;
const NOTIFY: u8 = 4;
const INDICATE: u8 = 8;
const ESTK_WRITE: u8 = 1;
const ESTK_NOTIFY: u8 = 2;
const NUS_WRITE: u8 = 4;
const NUS_NOTIFY: u8 = 8;
const ESTK_SERVICE: u8 = 1;
const NUS_SERVICE: u8 = 2;

#[derive(Clone)]
struct Characteristic {
    id: u32,
    uuid: String,
    properties: u8,
    matches: u8,
}
impl Characteristic {
    fn writable(&self) -> bool {
        self.properties & (WRITE | WRITE_WITHOUT_RESPONSE) != 0
    }
    fn notifiable(&self) -> bool {
        self.properties & (NOTIFY | INDICATE) != 0
    }
}

struct Selection {
    protocol: u8,
    remaining: usize,
    hinted_writer: Option<u32>,
    hinted_notifier: Option<u32>,
    fallback_writer: Option<u32>,
    fallback_notifier: Option<u32>,
    writer: Option<u32>,
    notifier: Option<u32>,
    service: u8,
    already_bound: bool,
    observations: Vec<Characteristic>,
    updates: bool,
    invalid: bool,
}

impl Selection {
    fn new(protocol: u8, services: usize) -> Self {
        Self {
            protocol,
            remaining: services,
            hinted_writer: None,
            hinted_notifier: None,
            fallback_writer: None,
            fallback_notifier: None,
            writer: None,
            notifier: None,
            service: 0,
            already_bound: false,
            observations: Vec::new(),
            updates: false,
            invalid: protocol > 3,
        }
    }

    fn missing_service(&mut self, error: i32) -> i32 {
        self.remaining = self.remaining.saturating_sub(1);
        if self.remaining == 0 { error } else { 0 }
    }

    fn finish(&mut self) -> i32 {
        self.updates = false;
        if self.invalid {
            return -127;
        }
        if self.protocol == 0 || self.protocol == 2 {
            for c in &self.observations {
                // Legacy's exact ESTK matches deliberately do not inspect
                // properties. Preserve its original if/else-if ordering,
                // including a duplicate hint's possible fallback admission.
                if self.protocol == 0
                    && c.matches & ESTK_NOTIFY != 0
                    && self.hinted_notifier.is_none()
                {
                    self.hinted_notifier = Some(c.id);
                } else if self.protocol == 0
                    && c.matches & ESTK_WRITE != 0
                    && self.hinted_writer.is_none()
                {
                    self.hinted_writer = Some(c.id);
                } else if c.uuid.to_lowercase().contains("ae01")
                    && c.writable()
                    && self.hinted_writer.is_none()
                {
                    self.hinted_writer = Some(c.id);
                } else if c.uuid.to_lowercase().contains("ae02")
                    && c.notifiable()
                    && self.hinted_notifier.is_none()
                {
                    self.hinted_notifier = Some(c.id);
                } else if c.writable() && self.fallback_writer.is_none() {
                    self.fallback_writer = Some(c.id);
                } else if c.notifiable() && self.fallback_notifier.is_none() {
                    self.fallback_notifier = Some(c.id);
                }
            }
            self.writer = self.hinted_writer.or(self.fallback_writer);
            self.notifier = self.hinted_notifier.or(self.fallback_notifier);
            self.updates = true; // Swift exposed partial fallback choices too.
            if self.writer.is_none() || self.notifier.is_none() {
                return self.missing_service(if self.protocol == 2 { -1 } else { -2 });
            }
            self.hinted_writer = None;
            self.hinted_notifier = None;
            self.fallback_writer = None;
            self.fallback_notifier = None;
        } else {
            if self.protocol == 1 && self.already_bound {
                return 2;
            }
            let (service, writer, notifier, missing_service, missing_pair) = if self.protocol == 1 {
                (ESTK_SERVICE, ESTK_WRITE, ESTK_NOTIFY, -3, -4)
            } else {
                (NUS_SERVICE, NUS_WRITE, NUS_NOTIFY, -5, -6)
            };
            if self.service & service == 0 {
                return self.missing_service(missing_service);
            }
            let writer = self
                .observations
                .iter()
                .find(|c| c.matches & writer != 0 && c.writable())
                .map(|c| c.id);
            let notifier = self
                .observations
                .iter()
                .find(|c| c.matches & notifier != 0 && c.notifiable())
                .map(|c| c.id);
            if writer.is_none() || notifier.is_none() {
                return missing_pair;
            }
            self.writer = writer;
            self.notifier = notifier;
            self.updates = true;
        }
        self.remaining = 0;
        1
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_gatt_selection_create(protocol: u8, services: usize) -> *mut c_void {
    Box::into_raw(Box::new(Selection::new(protocol, services))).cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Selection>()) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_begin(
    handle: *mut c_void,
    service_matches: u8,
    already_bound: u8,
) {
    if let Some(s) = unsafe { handle.cast::<Selection>().as_mut() } {
        s.invalid |= service_matches > 3 || already_bound > 1;
        s.service = service_matches;
        s.already_bound = already_bound != 0;
        s.observations.clear();
        s.updates = false;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_add(
    handle: *mut c_void,
    id: u32,
    uuid: *const u8,
    length: usize,
    properties: u8,
    matches: u8,
) -> u8 {
    let Some(s) = (unsafe { handle.cast::<Selection>().as_mut() }) else {
        return 0;
    };
    if s.invalid {
        return 0;
    }
    if id == 0
        || length > 1024
        || properties > 15
        || matches > 15
        || (length != 0 && (uuid.is_null() || (uuid as usize).checked_add(length).is_none()))
    {
        s.invalid = true;
        return 0;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(uuid, length) }
    };
    let Ok(uuid) = std::str::from_utf8(bytes) else {
        s.invalid = true;
        return 0;
    };
    s.observations.push(Characteristic {
        id,
        uuid: uuid.into(),
        properties,
        matches,
    });
    1
}
/// 0 wait; 1 ready; 2 retain existing CCID pair; negatives are unchanged
/// adapter error branches: BeeSIM, legacy, CCID service/pair, NUS service/pair.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_finish(handle: *mut c_void) -> i32 {
    unsafe { handle.cast::<Selection>().as_mut() }.map_or(-127, Selection::finish)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_updates(handle: *const c_void) -> u8 {
    unsafe { handle.cast::<Selection>().as_ref() }.map_or(0, |s| u8::from(s.updates))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_pair(handle: *const c_void) -> u64 {
    unsafe { handle.cast::<Selection>().as_ref() }.map_or(0, |s| {
        (u64::from(s.writer.unwrap_or(0)) << 32) | u64::from(s.notifier.unwrap_or(0))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_gatt_selection_remaining(handle: *const c_void) -> usize {
    unsafe { handle.cast::<Selection>().as_ref() }.map_or(0, |s| s.remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(id: u32, uuid: &str, properties: u8, matches: u8) -> Characteristic {
        Characteristic {
            id,
            uuid: uuid.into(),
            properties,
            matches,
        }
    }
    fn observe(s: &mut Selection, service: u8, cs: Vec<Characteristic>) -> i32 {
        s.service = service;
        s.observations = cs;
        s.finish()
    }
    #[test]
    fn beesim_uses_first_usable_pair_before_later_hints() {
        let mut s = Selection::new(2, 2);
        assert_eq!(
            observe(
                &mut s,
                0,
                vec![c(1, "1111", WRITE, 0), c(2, "2222", NOTIFY, 0)]
            ),
            1
        );
        assert_eq!((s.writer, s.notifier, s.remaining), (Some(1), Some(2), 0));
    }
    #[test]
    fn partial_candidates_accumulate_and_hints_override_fallbacks() {
        let mut s = Selection::new(2, 2);
        assert_eq!(observe(&mut s, 0, vec![c(1, "1111", WRITE, 0)]), 0);
        assert!(s.updates);
        assert_eq!(
            observe(
                &mut s,
                0,
                vec![
                    c(2, "XXae01XX", WRITE_WITHOUT_RESPONSE, 0),
                    c(3, "AE02", INDICATE, 0)
                ]
            ),
            1
        );
        assert_eq!((s.writer, s.notifier), (Some(2), Some(3)));
    }
    #[test]
    fn dual_property_characteristics_follow_original_else_if_order() {
        let mut s = Selection::new(2, 1);
        assert_eq!(
            observe(&mut s, 0, vec![c(1, "1234", WRITE | NOTIFY, 0)]),
            -1
        );
        assert_eq!((s.writer, s.notifier), (Some(1), None));
        let mut s = Selection::new(2, 1);
        assert_eq!(
            observe(
                &mut s,
                0,
                vec![
                    c(1, "1234", WRITE | NOTIFY, 0),
                    c(2, "1234", WRITE | NOTIFY, 0)
                ]
            ),
            1
        );
        assert_eq!((s.writer, s.notifier), (Some(1), Some(2)));
    }
    #[test]
    fn legacy_exact_hints_do_not_require_properties_and_first_duplicate_wins() {
        let mut s = Selection::new(0, 1);
        assert_eq!(
            observe(
                &mut s,
                0,
                vec![
                    c(1, "544B", 0, ESTK_NOTIFY),
                    c(2, "6D65", 0, ESTK_WRITE),
                    c(3, "6D65", WRITE, ESTK_WRITE)
                ]
            ),
            1
        );
        assert_eq!((s.writer, s.notifier), (Some(2), Some(1)));
    }
    #[test]
    fn ccid_and_nus_require_exact_service_pair_and_properties() {
        for (proto, svc, w, n, service_error, pair_error) in [
            (1, ESTK_SERVICE, ESTK_WRITE, ESTK_NOTIFY, -3, -4),
            (3, NUS_SERVICE, NUS_WRITE, NUS_NOTIFY, -5, -6),
        ] {
            let mut s = Selection::new(proto, 2);
            assert_eq!(
                observe(
                    &mut s,
                    0,
                    vec![c(1, "any", WRITE, w), c(2, "any", NOTIFY, n)]
                ),
                0
            );
            assert!(!s.updates);
            assert_eq!(observe(&mut s, 0, vec![]), service_error);
            let mut s = Selection::new(proto, 2);
            assert_eq!(
                observe(&mut s, svc, vec![c(1, "any", 0, w), c(2, "any", NOTIFY, n)]),
                pair_error
            );
            let mut s = Selection::new(proto, 1);
            assert_eq!(
                observe(
                    &mut s,
                    svc,
                    vec![
                        c(1, "any", WRITE, w),
                        c(2, "any", NOTIFY, n),
                        c(3, "any", WRITE, w)
                    ]
                ),
                1
            );
            assert_eq!((s.writer, s.notifier), (Some(1), Some(2)));
        }
    }
    #[test]
    fn ccid_keeps_previously_bound_pair_and_wait_count_is_not_decremented_on_ready() {
        let mut s = Selection::new(1, 3);
        s.already_bound = true;
        assert_eq!(observe(&mut s, 0, vec![]), 2);
        assert_eq!(s.remaining, 3);
        assert!(!s.updates);
    }
    #[test]
    fn malformed_ffi_input_poisoning_is_sticky_and_null_safe() {
        unsafe {
            assert_eq!(switch_ble_gatt_selection_finish(std::ptr::null_mut()), -127);
            switch_ble_gatt_selection_destroy(std::ptr::null_mut());
            let h = switch_ble_gatt_selection_create(2, 1);
            switch_ble_gatt_selection_begin(h, 0, 0);
            assert_eq!(
                switch_ble_gatt_selection_add(h, 1, std::ptr::null(), 1, WRITE, 0),
                0
            );
            assert_eq!(switch_ble_gatt_selection_finish(h), -127);
            switch_ble_gatt_selection_destroy(h);
        }
    }
}
