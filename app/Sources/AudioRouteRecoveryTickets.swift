import Foundation

/// Queue-confined generation fence for delayed CoreAudio route-recovery work.
/// Time is supplied by the caller, so retries can be exercised deterministically.
struct AudioRouteRecoveryTicket: Equatable {
    let sessionGeneration: UInt64
    let recoveryGeneration: UInt64
}

struct AudioRouteRecoveryTicketLedger {
    private(set) var sessionGeneration: UInt64 = 0
    private(set) var recoveryGeneration: UInt64 = 0

    /// Retire the current call session and every retry it scheduled.
    mutating func retireSession() {
        sessionGeneration &+= 1
        recoveryGeneration &+= 1
    }

    /// Claim a route recovery, invalidating retries queued by older failures.
    mutating func claimRecovery() {
        recoveryGeneration &+= 1
    }

    /// Create the identity captured by a delayed retry.
    mutating func scheduleRetry() -> AudioRouteRecoveryTicket {
        recoveryGeneration &+= 1
        return AudioRouteRecoveryTicket(
            sessionGeneration: sessionGeneration,
            recoveryGeneration: recoveryGeneration
        )
    }

    func isCurrent(_ ticket: AudioRouteRecoveryTicket, running: Bool) -> Bool {
        running
            && ticket.sessionGeneration == sessionGeneration
            && ticket.recoveryGeneration == recoveryGeneration
    }
}
