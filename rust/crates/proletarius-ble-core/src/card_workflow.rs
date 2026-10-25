//! Pure card-channel setup/reset workflow. The adapter executes one command
//! and supplies its transport outcome. It does not select protocol commands or
//! interpret mode success. Raw response bytes remain owned by the caller.
use crate::{ReaderProtocol, ccid_claim_continuation, input_slice, output_slice};
use std::ffi::c_void;

const CLAIM_CCID: &[u8] = &[
    0x6f, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0, 2, 0, 0,
];
const POWER_CCID: &[u8] = &[0x62, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const OFF_CCID: &[u8] = &[0x63, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const CLAIM_LEGACY: &[u8] = &[2, 6, 0, 0x45, 0x53, 0x54, 0x4b, 0x6d, 0x65];
const POWER_LEGACY: &[u8] = &[3, 2, 0, 1, 1];
const OFF_LEGACY: &[u8] = &[3, 0, 0];
const BEE_INIT: &[u8] = &[0xa0, 0x3e, 4, 0, 0];
const BEE_RESET: &[u8] = &[0xa0, 0x3f, 0, 0, 0];
const MODE: &[u8] = &[0xfe, 0x0d, 0, 0, 1];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Claim,
    Continuation,
    Power,
    Mode,
    Off,
    ResetOn,
    BeeInit,
    BeeReset,
    Terminal(i32),
}

struct Workflow {
    protocol: ReaderProtocol,
    stage: Stage,
    step: u64,
    continuation: Vec<u8>,
    optional_failed: bool,
}
impl Workflow {
    fn new(protocol: u8, operation: u8) -> Option<Self> {
        let protocol = match protocol {
            0 => ReaderProtocol::Legacy,
            1 => ReaderProtocol::Ccid,
            2 => ReaderProtocol::BeeSim,
            3 => ReaderProtocol::NineEsim,
            _ => return None,
        };
        let stage = match (operation, protocol) {
            (1, ReaderProtocol::Legacy | ReaderProtocol::Ccid) => Stage::Claim,
            (1, ReaderProtocol::BeeSim) => Stage::BeeInit,
            (1, ReaderProtocol::NineEsim) => Stage::Mode,
            (2, ReaderProtocol::Legacy | ReaderProtocol::Ccid) => Stage::Off,
            (2, ReaderProtocol::BeeSim) => Stage::BeeReset,
            (2, ReaderProtocol::NineEsim) => Stage::Terminal(-15),
            _ => return None,
        };
        Some(Self {
            protocol,
            stage,
            step: 1,
            continuation: Vec::new(),
            optional_failed: false,
        })
    }
    fn status(&self) -> i32 {
        match self.stage {
            Stage::Terminal(code) => code,
            Stage::Mode => 2,
            Stage::Power if self.optional_failed => 5,
            Stage::Continuation => 6,
            _ => 1,
        }
    }
    fn command(&self) -> &[u8] {
        let ccid = self.protocol == ReaderProtocol::Ccid;
        match self.stage {
            Stage::Claim => {
                if ccid {
                    CLAIM_CCID
                } else {
                    CLAIM_LEGACY
                }
            }
            Stage::Continuation => &self.continuation,
            Stage::Power | Stage::ResetOn => {
                if ccid {
                    POWER_CCID
                } else {
                    POWER_LEGACY
                }
            }
            Stage::Mode => MODE,
            Stage::Off => {
                if ccid {
                    OFF_CCID
                } else {
                    OFF_LEGACY
                }
            }
            Stage::BeeInit => BEE_INIT,
            Stage::BeeReset => BEE_RESET,
            Stage::Terminal(_) => &[],
        }
    }
    fn advance(&mut self, step: u64, success: bool, response: &[u8]) -> bool {
        if step != self.step || matches!(self.stage, Stage::Terminal(_)) {
            return false;
        }
        self.stage = match self.stage {
            Stage::Claim if !success => Stage::Terminal(-10),
            Stage::Claim => {
                if self.protocol == ReaderProtocol::Ccid {
                    if let Some(command) = ccid_claim_continuation(response) {
                        self.continuation = command;
                        Stage::Continuation
                    } else {
                        Stage::Power
                    }
                } else {
                    Stage::Power
                }
            }
            // Reviewed old behavior: this negotiation is optional. Even a
            // transport failure proceeds to the power request; the physical
            // adapter must still reject writing on a revoked GATT channel.
            Stage::Continuation => {
                self.optional_failed = !success;
                Stage::Power
            }
            Stage::Power => Stage::Terminal(if success { 3 } else { -11 }),
            Stage::BeeInit => Stage::Terminal(if success { 3 } else { -12 }),
            Stage::Mode => Stage::Terminal(if !success {
                -13
            } else if response == [2, 0x90, 0] {
                3
            } else {
                -14
            }),
            Stage::Off if success => Stage::ResetOn,
            Stage::Off | Stage::ResetOn | Stage::BeeReset => {
                Stage::Terminal(if success { 4 } else { -16 })
            }
            Stage::Terminal(_) => return false,
        };
        // A workflow has at most three writes; never wrap a response token.
        self.step += 1;
        true
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_ble_card_workflow_create(protocol: u8, operation: u8) -> *mut c_void {
    Workflow::new(protocol, operation)
        .map_or(std::ptr::null_mut(), |s| Box::into_raw(Box::new(s)).cast())
}
/// # Safety
/// Null or an exclusively owned live handle; destroy each non-null handle once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_card_workflow_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Workflow>()) });
    }
}
/// 1=raw command, 2=APDU command, 3=ready, 4=reset complete,
/// 5=raw command after optional failure, 6=optional continuation raw command
/// (both add diagnostics only); negative=failure.
/// # Safety
/// A non-null handle is live and has no concurrent mutable access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_card_workflow_status(handle: *const c_void) -> i32 {
    unsafe { handle.cast::<Workflow>().as_ref() }.map_or(-17, Workflow::status)
}
/// # Safety
/// A non-null handle is live and has no concurrent mutable access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_card_workflow_step(handle: *const c_void) -> u64 {
    unsafe { handle.cast::<Workflow>().as_ref() }.map_or(0, |s| s.step)
}
/// # Safety
/// A non-null handle is live; output denotes capacity writable bytes and must
/// not alias the handle or its memory. No concurrent access to the output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_card_workflow_command(
    handle: *const c_void,
    output: *mut u8,
    capacity: usize,
) -> isize {
    let Some(state) = (unsafe { handle.cast::<Workflow>().as_ref() }) else {
        return -1;
    };
    let Some(output) = output_slice(output, capacity) else {
        return -1;
    };
    let command = state.command();
    if command.len() > output.len() {
        return -2;
    }
    output[..command.len()].copy_from_slice(command);
    command.len() as isize
}
/// # Safety
/// A non-null handle is exclusively owned and live; response denotes length
/// readable bytes and must not alias the handle or its memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_card_workflow_advance(
    handle: *mut c_void,
    step: u64,
    success: u8,
    response: *const u8,
    length: usize,
) -> u8 {
    let Some(response) = input_slice(response, length) else {
        return 0;
    };
    let Some(state) = (unsafe { handle.cast::<Workflow>().as_mut() }) else {
        return 0;
    };
    u8::from(state.advance(step, success != 0, response))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn advance(state: &mut Workflow, success: bool, response: &[u8]) {
        assert!(state.advance(state.step, success, response));
    }
    #[test]
    fn setup_commands_match_production_swift_golden() {
        for (protocol, claim, power) in [
            (0, CLAIM_LEGACY, POWER_LEGACY),
            (1, CLAIM_CCID, POWER_CCID),
            (2, BEE_INIT, &[][..]),
        ] {
            let mut s = Workflow::new(protocol, 1).unwrap();
            assert_eq!(s.command(), claim);
            advance(&mut s, true, &[]);
            if protocol != 2 {
                assert_eq!(s.command(), power);
                advance(&mut s, true, &[]);
            }
            assert_eq!(s.status(), 3);
        }
    }
    #[test]
    fn optional_claim_success_and_failure_both_power_on() {
        for success in [true, false] {
            let mut s = Workflow::new(1, 1).unwrap();
            advance(&mut s, true, &[0x80, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x61, 0x10]);
            assert_eq!(
                s.command(),
                [0x6f, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 1, 0x10]
            );
            assert_eq!(s.status(), 6);
            let old = s.step;
            advance(&mut s, success, &[]);
            assert_eq!(s.command(), POWER_CCID);
            assert_eq!(s.status(), if success { 1 } else { 5 });
            assert!(!s.advance(old, true, &[]));
            assert_eq!(s.command(), POWER_CCID);
            advance(&mut s, true, &[]);
            assert_eq!(s.status(), 3);
        }
    }
    #[test]
    fn exact_management_mode_and_transport_failure_gate_ready() {
        for response in [&[2, 0x90, 0][..], &[1, 0x90, 0], &[2, 0x90, 0, 0], &[]] {
            let mut s = Workflow::new(3, 1).unwrap();
            assert_eq!(s.status(), 2);
            assert_eq!(s.command(), MODE);
            advance(&mut s, true, response);
            assert_eq!(s.status(), if response == [2, 0x90, 0] { 3 } else { -14 });
        }
        let mut s = Workflow::new(3, 1).unwrap();
        advance(&mut s, false, &[2, 0x90, 0]);
        assert_eq!(s.status(), -13);
    }
    #[test]
    fn reset_commands_and_unsupported_b1_match_golden() {
        for (p, off, on) in [
            (0, OFF_LEGACY, POWER_LEGACY),
            (1, OFF_CCID, POWER_CCID),
            (2, BEE_RESET, &[][..]),
        ] {
            let mut s = Workflow::new(p, 2).unwrap();
            assert_eq!(s.command(), off);
            advance(&mut s, true, &[]);
            if p != 2 {
                assert_eq!(s.command(), on);
                advance(&mut s, true, &[]);
            }
            assert_eq!(s.status(), 4);
        }
        let s = Workflow::new(3, 2).unwrap();
        assert_eq!(s.status(), -15);
        assert!(s.command().is_empty());
    }
    #[test]
    fn failures_and_duplicate_responses_never_advance_twice() {
        for p in 0..3 {
            let mut s = Workflow::new(p, 1).unwrap();
            advance(&mut s, false, &[]);
            assert!(s.status() < 0);
            assert!(!s.advance(s.step, true, &[]));
            let mut s = Workflow::new(p, 2).unwrap();
            advance(&mut s, false, &[]);
            assert_eq!(s.status(), -16);
            assert!(s.command().is_empty());
            if p != 2 {
                let mut s = Workflow::new(p, 1).unwrap();
                advance(&mut s, true, &[]);
                advance(&mut s, false, &[]);
                assert_eq!(s.status(), -11);
            }
        }
        assert!(Workflow::new(9, 1).is_none());
        assert!(Workflow::new(1, 9).is_none());
    }
}
