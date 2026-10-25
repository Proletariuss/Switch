import Foundation

@_silgen_name("switch_ble_deadline_create") private func switchBLEDeadlineCreate() -> UnsafeMutableRawPointer?
@_silgen_name("switch_ble_deadline_destroy") private func switchBLEDeadlineDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_ble_deadline_arm") private func switchBLEDeadlineArm(_ handle: UnsafeMutableRawPointer, _ phase: UInt8, _ target: UnsafePointer<UInt8>?, _ length: Int) -> UInt64
@_silgen_name("switch_ble_deadline_duration_ms") private func switchBLEDeadlineDurationMS(_ handle: UnsafeMutableRawPointer, _ ticket: UInt64) -> UInt64
@_silgen_name("switch_ble_deadline_fire") private func switchBLEDeadlineFire(_ handle: UnsafeMutableRawPointer, _ ticket: UInt64, _ phase: UInt8, _ target: UnsafePointer<UInt8>?, _ length: Int) -> UInt8
@_silgen_name("switch_ble_deadline_cancel") private func switchBLEDeadlineCancel(_ handle: UnsafeMutableRawPointer)

enum BLEOwnerPhase: UInt8 {
    case discovery = 1, centralInitialization, connecting, serviceDiscovery
    case characteristicDiscovery, notification, apdu, permissionObservation
}

enum BLEOwnerTimeoutAction: UInt8 {
    case none = 0, stopDiscovery, failInitialization, failConnectionPhase
    case expireAPDUObservation, observePermissionAgain
}

/// Platform timer adapter for a Rust-owned deadline ticket. Swift schedules
/// the wake-up, but it cannot choose a duration or turn a stale callback into
/// a transport/business terminal state.
final class BLEPhaseDeadline {
    private let queue: DispatchQueue
    private let handle: UnsafeMutableRawPointer
    private var work: DispatchWorkItem?

    init(queue: DispatchQueue = .main) {
        guard let handle = switchBLEDeadlineCreate() else { preconditionFailure("Rust BLE deadline owner unavailable") }
        self.queue = queue
        self.handle = handle
    }
    deinit { switchBLEDeadlineDestroy(handle) }

    private func withTarget<T>(_ target: String, _ body: (UnsafePointer<UInt8>?, Int) -> T) -> T {
        Data(target.utf8).withUnsafeBytes { raw in body(raw.bindMemory(to: UInt8.self).baseAddress, raw.count) }
    }

    @discardableResult
    func arm(phase: BLEOwnerPhase, target: String = "", onAction: @escaping (BLEOwnerTimeoutAction) -> Void) -> UInt64 {
        cancel()
        let ticket = withTarget(target) { switchBLEDeadlineArm(handle, phase.rawValue, $0, $1) }
        guard ticket != 0 else { return 0 }
        let durationMS = switchBLEDeadlineDurationMS(handle, ticket)
        guard durationMS != 0 else { return 0 }
        let next = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.work = nil
            let raw = self.withTarget(target) { switchBLEDeadlineFire(self.handle, ticket, phase.rawValue, $0, $1) }
            guard let action = BLEOwnerTimeoutAction(rawValue: raw), action != .none else { return }
            onAction(action)
        }
        work = next
        queue.asyncAfter(deadline: .now() + (Double(durationMS) / 1_000), execute: next)
        return ticket
    }

    func cancel() {
        switchBLEDeadlineCancel(handle)
        work?.cancel()
        work = nil
    }
}

@_silgen_name("switch_ble_target_create") private func switchBLETargetCreate() -> UnsafeMutableRawPointer?
@_silgen_name("switch_ble_target_destroy") private func switchBLETargetDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_ble_target_begin") private func switchBLETargetBegin(_ handle: UnsafeMutableRawPointer, _ target: UnsafePointer<UInt8>?, _ length: Int) -> UInt64
@_silgen_name("switch_ble_target_observe") private func switchBLETargetObserve(_ handle: UnsafeMutableRawPointer, _ generation: UInt64, _ target: UnsafePointer<UInt8>?, _ length: Int) -> UInt8
@_silgen_name("switch_ble_target_explicit") private func switchBLETargetExplicit(_ handle: UnsafeMutableRawPointer, _ target: UnsafePointer<UInt8>?, _ length: Int) -> UInt8
@_silgen_name("switch_ble_target_may_persist") private func switchBLETargetMayPersist(_ handle: UnsafeMutableRawPointer, _ target: UnsafePointer<UInt8>?, _ length: Int) -> UInt8
@_silgen_name("switch_ble_target_stop") private func switchBLETargetStop(_ handle: UnsafeMutableRawPointer)

/// UserDefaults supplies bytes only; Rust decides whether a fresh
/// advertisement is the remembered target and which ready target may persist.
final class BLETargetSelectionPolicy {
    private let handle: UnsafeMutableRawPointer
    private var generation: UInt64 = 0
    init() {
        guard let handle = switchBLETargetCreate() else { preconditionFailure("Rust BLE target owner unavailable") }
        self.handle = handle
    }
    deinit { switchBLETargetDestroy(handle) }
    private func withValue<T>(_ value: String, _ body: (UnsafePointer<UInt8>?, Int) -> T) -> T {
        Data(value.utf8).withUnsafeBytes { raw in body(raw.bindMemory(to: UInt8.self).baseAddress, raw.count) }
    }
    func beginScan(rememberedBytes: String?) {
        generation = withValue(rememberedBytes ?? "") { switchBLETargetBegin(handle, $0, $1) }
    }
    func acceptsObservedCandidate(_ id: UUID) -> Bool {
        withValue(id.uuidString) { switchBLETargetObserve(handle, generation, $0, $1) == 1 }
    }
    func selectExplicitly(_ id: UUID) -> Bool {
        withValue(id.uuidString) { switchBLETargetExplicit(handle, $0, $1) == 1 }
    }
    func mayPersist(_ id: UUID) -> Bool {
        withValue(id.uuidString) { switchBLETargetMayPersist(handle, $0, $1) == 1 }
    }
    func stop() { switchBLETargetStop(handle) }
}
