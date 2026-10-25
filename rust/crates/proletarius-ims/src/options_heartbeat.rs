//! Shared OPTIONS fail-confirm policy for native and compatibility transports.
//! A provisional response cannot prove liveness. One missed primary probe is
//! confirmed after three seconds; only a final 2xx response restores health.
use std::time::Duration;

pub const CONFIRM_DELAY: Duration = Duration::from_secs(3);
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Continue,
    ConfirmAfterDelay,
    Recovered,
    Teardown,
}
impl Decision {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::ConfirmAfterDelay => "confirm_after_delay",
            Self::Recovered => "recovered",
            Self::Teardown => "teardown",
        }
    }
    pub const fn retry_after(self) -> Duration {
        if matches!(self, Self::ConfirmAfterDelay) {
            CONFIRM_DELAY
        } else {
            Duration::ZERO
        }
    }
}
pub const fn decide(confirmation: bool, status: u16) -> Decision {
    match (confirmation, status >= 200 && status < 300) {
        (false, true) => Decision::Continue,
        (false, false) => Decision::ConfirmAfterDelay,
        (true, true) => Decision::Recovered,
        (true, false) => Decision::Teardown,
    }
}
