import Foundation

@main
private enum BLEPhaseDeadlineTests {
    static func main() {
        let queue = DispatchQueue(label: "ble-phase-deadline-tests")
        let deadline = BLEPhaseDeadline(queue: queue)
        let expired = DispatchSemaphore(value: 0)

        deadline.arm(phase: .permissionObservation) { action in
            precondition(action == .observePermissionAgain)
            expired.signal()
        }
        precondition(expired.wait(timeout: .now() + 2) == .success, "Rust-owned deadline did not fire")

        let cancelled = DispatchSemaphore(value: 0)
        deadline.arm(phase: .permissionObservation) { _ in cancelled.signal() }
        deadline.cancel()
        precondition(cancelled.wait(timeout: .now() + 1.2) == .timedOut, "cancelled deadline fired")

        let replacement = DispatchSemaphore(value: 0)
        var count = 0
        deadline.arm(phase: .permissionObservation, target: "old") { _ in count += 10; replacement.signal() }
        deadline.arm(phase: .permissionObservation, target: "new") { _ in count += 1; replacement.signal() }
        precondition(replacement.wait(timeout: .now() + 2) == .success, "replacement deadline did not fire")
        precondition(count == 1, "superseded deadline fired after replacement")

        // Repeated phase changes must leave only the final deadline armed.
        for _ in 0..<100 {
            deadline.arm(phase: .permissionObservation) { _ in count += 100 }
            deadline.cancel()
        }
        let settled = DispatchSemaphore(value: 0)
        queue.asyncAfter(deadline: .now() + 1.2) { settled.signal() }
        precondition(settled.wait(timeout: .now() + 2) == .success, "deadline stress wait timed out")
        precondition(count == 1, "cancel stress leaked a deadline callback")
        print("BLE phase deadline: expiry, cancel, replace, and 100-cycle cancel stress passed")
    }
}
