import Foundation
import CoreAudio

private final class HALFixture {
    var failCreate = 0
    var failStart = 0
    var failStop: AudioDeviceID = 0
    var failDestroy: AudioDeviceID = 0
    var cookies: [UnsafeMutableRawPointer] = []
    var starts = 0
    var stops = 0
    var destroys = 0
    var stopEntered: DispatchSemaphore?

    func install() {
        AudioBridge.shared.lifetimeTestInstall(.init(
            create: { [self] _, cookie in
                cookies.append(cookie)
                if cookies.count == failCreate { return (-77, nil) }
                let proc: AudioDeviceIOProcID = { _, _, _, _, _, _, _ in noErr }
                return (noErr, proc)
            },
            start: { [self] _, _ in
                starts += 1
                // HAL can enter a callback before AudioDeviceStart returns.
                precondition(AudioBridge.lifetimeTestInvoke(cookies.last!) == noErr)
                return starts == failStart ? -88 : noErr
            },
            stop: { [self] device, _ in
                stops += 1
                stopEntered?.signal()
                return device == failStop ? -99 : noErr
            },
            destroy: { [self] device, _ in
                destroys += 1
                return device == failDestroy ? -111 : noErr
            }))
    }
}

@main
private struct AudioIOProcLifetimeTests {
    static let bridge = AudioBridge.shared

    static func main() {
        precondition(MemoryLayout<UInt>.size == 8)
        precondition(MemoryLayout<UnsafeMutableRawPointer>.size == 8)
#if !arch(arm64)
        fatalError("This proof harness is frozen to the application's arm64 macOS target")
#endif
        cookieRoundTripsAndRejectsUnknownValues()
        generationsNeverReuseOrOverflow()
        callbackActivationPrecedesEveryStart()
        partialRegistrationAndSecondStartFailureCloseGeneration()
        failedCleanupRetainsOwnershipUntilRetry()
        destroyFailureClosesGenerationAndBlocksReplacement()
        delayedPreEntryCannotAccessSuccessor()
        enteredCallbackDrainsBeforeReset()
        retirementLedgerRetainsARCResourceAcrossFailure()
        print("PASS: 9 IOProc lifetime groups; 4-role/zero/overflow cookies, real adapter pre-entry/in-flight ordering, partial registration/start/stop/destroy failures, successor isolation and ARC ledger ownership. Mock HAL only; no audio device, microphone or network.")
    }

    static func cookieRoundTripsAndRejectsUnknownValues() {
        for generation in [UInt(1), 2, 12345, AudioIOProcCookie.maximumGeneration] {
            var unique = Set<UInt>()
            for role in 0..<4 {
                let cookie = AudioIOProcCookie(generation: generation, role: role)!
                let bits = UInt(bitPattern: cookie.opaque)
                precondition(bits >> 2 == generation && Int(bits & 3) == role)
                precondition(Int(bitPattern: cookie.opaque) == Int(bits))
                precondition(UInt(UInt32(exactly: bits)!) == bits)
                precondition(AudioIOProcCookie(opaque: cookie.opaque) == cookie)
                precondition(unique.insert(bits).inserted)
            }
            precondition(unique.count == 4)
        }
        for role in 0..<4 { precondition(AudioIOProcCookie(generation: 0, role: role) == nil) }
        precondition(AudioIOProcCookie(generation: 1, role: -1) == nil)
        precondition(AudioIOProcCookie(generation: 1, role: 4) == nil)
        precondition(AudioIOProcCookie(generation: AudioIOProcCookie.maximumGeneration + 1, role: 0) == nil)
        precondition(AudioIOProcCookie(opaque: nil) == nil)
        for bits: UInt in [1, 2, 3, UInt(UInt32.max) + 1] {
            // Every test bit pattern remains canonical on the frozen target;
            // it is an opaque value, never a readable memory address.
            precondition(AudioIOProcCookie(opaque: UnsafeMutableRawPointer(bitPattern: bits)) == nil)
            precondition(AudioBridge.lifetimeTestInvoke(UnsafeMutableRawPointer(bitPattern: bits)) == noErr)
        }
        precondition(AudioBridge.lifetimeTestInvoke(nil) == noErr)
        precondition(bridge.lifetimeTestSnapshot().accesses == 0)
    }

    static func generationsNeverReuseOrOverflow() {
        var fence = AudioIOProcGenerationFence(lastGeneration: AudioIOProcCookie.maximumGeneration - 1)
        let final = fence.begin()!
        precondition(final == AudioIOProcCookie.maximumGeneration)
        for role in 0..<4 { precondition(fence.admits(AudioIOProcCookie(generation: final, role: role)!)) }
        precondition(fence.begin() == nil)
        fence.deactivate()
        precondition(fence.begin() == nil && fence.activeGeneration == nil)
        precondition(fence.lastGeneration == final)
        var invalid = AudioIOProcGenerationFence(lastGeneration: UInt.max)
        precondition(invalid.begin() == nil && invalid.activeGeneration == nil)
    }

    static func callbackActivationPrecedesEveryStart() {
        let hal = HALFixture()
        hal.install()
        let before = bridge.lifetimeTestSnapshot().accesses
        precondition(bridge.lifetimeTestStartIO() == nil)
        precondition(hal.starts == 4 && hal.cookies.count == 4)
        let active = bridge.lifetimeTestSnapshot()
        precondition(active.generation != nil && active.accesses == before + 4)
        precondition(active.bytes == 1 && active.registrations == 4)
        precondition(bridge.isRunning && !active.retirementPending)
        precondition(bridge.stop() == nil)
        let stopped = bridge.lifetimeTestSnapshot()
        precondition(stopped.generation == nil && stopped.bytes == 0 && stopped.registrations == 0)
        precondition(!bridge.isRunning && !stopped.retirementPending)
        for cookie in hal.cookies { precondition(AudioBridge.lifetimeTestInvoke(cookie) == noErr) }
        precondition(bridge.lifetimeTestSnapshot().accesses == stopped.accesses)
    }

    static func partialRegistrationAndSecondStartFailureCloseGeneration() {
        for (create, start) in [(1, 0), (2, 0), (0, 2), (0, 3)] {
            let hal = HALFixture()
            hal.failCreate = create
            hal.failStart = start
            hal.install()
            precondition(bridge.lifetimeTestStartIO() != nil)
            let state = bridge.lifetimeTestSnapshot()
            precondition(state.generation == nil && state.bytes == 0 && state.registrations == 0)
            precondition(!bridge.isRunning && !state.retirementPending)
            for cookie in hal.cookies { precondition(AudioBridge.lifetimeTestInvoke(cookie) == noErr) }
            precondition(bridge.lifetimeTestSnapshot().accesses == state.accesses)
        }
    }

    static func failedCleanupRetainsOwnershipUntilRetry() {
        for failSecondStart in [false, true] {
            let hal = HALFixture()
            hal.failStart = failSecondStart ? 2 : 0
            hal.failStop = failSecondStart ? 1 : 2
            hal.install()
            let start = bridge.lifetimeTestStartIO()
            precondition((start != nil) == failSecondStart)
            if !failSecondStart { precondition(bridge.stop() != nil) }
            let held = bridge.lifetimeTestSnapshot()
            precondition(held.generation != nil && held.bytes == 1 && held.registrations == 1)
            precondition(bridge.isRunning && held.retirementPending)
            precondition(bridge.lifetimeTestStartIO() != nil)
            precondition(bridge.lifetimeTestSnapshot().generation == held.generation)
            precondition(AudioBridge.lifetimeTestInvoke(hal.cookies[0]) == noErr)
            precondition(bridge.lifetimeTestSnapshot().accesses == held.accesses + 1)
            hal.failStop = 0
            precondition(bridge.stop() == nil)
            let stopped = bridge.lifetimeTestSnapshot()
            precondition(stopped.generation == nil && stopped.bytes == 0 && stopped.registrations == 0)
            precondition(!bridge.isRunning && !stopped.retirementPending)
        }
    }

    static func destroyFailureClosesGenerationAndBlocksReplacement() {
        for failSecondStart in [false, true] {
            let hal = HALFixture()
            hal.failStart = failSecondStart ? 2 : 0
            hal.failDestroy = 2
            hal.install()
            precondition((bridge.lifetimeTestStartIO() != nil) == failSecondStart)
            if !failSecondStart { precondition(bridge.stop() != nil) }
            // On second-start failure the first started proc is stopped, while
            // the created-but-unstarted second proc goes straight to destroy.
            precondition(hal.stops == (failSecondStart ? 1 : 4))
            let held = bridge.lifetimeTestSnapshot()
            precondition(held.generation == nil && held.bytes == 0 && held.registrations == 1)
            precondition(!bridge.isRunning && held.retirementPending)
            precondition(bridge.lifetimeTestStartIO() != nil)
            for cookie in hal.cookies { precondition(AudioBridge.lifetimeTestInvoke(cookie) == noErr) }
            precondition(bridge.lifetimeTestSnapshot().accesses == held.accesses)
            let acknowledgedStops = hal.stops
            hal.failDestroy = 0
            precondition(bridge.stop() == nil)
            precondition(hal.stops == acknowledgedStops)
            precondition(bridge.lifetimeTestSnapshot().registrations == 0)
        }
    }

    static func delayedPreEntryCannotAccessSuccessor() {
        let old = HALFixture()
        old.install()
        precondition(bridge.lifetimeTestStartIO() == nil)
        let bits = UInt(bitPattern: old.cookies[0])
        let ready = DispatchSemaphore(value: 0)
        let enter = DispatchSemaphore(value: 0)
        let finished = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            ready.signal()
            precondition(enter.wait(timeout: .now() + 5) == .success)
            precondition(AudioBridge.lifetimeTestInvoke(UnsafeMutableRawPointer(bitPattern: bits)) == noErr)
            finished.signal()
        }
        precondition(ready.wait(timeout: .now() + 5) == .success)
        precondition(bridge.stop() == nil)
        let next = HALFixture()
        next.install()
        precondition(bridge.lifetimeTestStartIO() == nil)
        let successor = bridge.lifetimeTestSnapshot()
        precondition(successor.generation != AudioIOProcCookie(opaque: old.cookies[0])!.generation)
        enter.signal()
        precondition(finished.wait(timeout: .now() + 5) == .success)
        let after = bridge.lifetimeTestSnapshot()
        precondition(after.generation == successor.generation && after.accesses == successor.accesses)
        precondition(after.bytes == successor.bytes && after.registrations == successor.registrations)
        precondition(bridge.stop() == nil)
    }

    static func enteredCallbackDrainsBeforeReset() {
        let hal = HALFixture()
        hal.install()
        precondition(bridge.lifetimeTestStartIO() == nil)
        let bits = UInt(bitPattern: hal.cookies[0])
        let entered = DispatchSemaphore(value: 0)
        let leave = DispatchSemaphore(value: 0)
        let callbackDone = DispatchSemaphore(value: 0)
        let stopDone = DispatchSemaphore(value: 0)
        let halStopEntered = DispatchSemaphore(value: 0)
        hal.stopEntered = halStopEntered
        bridge.lifetimeTestSetInsidePipeline {
            entered.signal()
            precondition(leave.wait(timeout: .now() + 5) == .success)
        }
        DispatchQueue.global().async {
            precondition(AudioBridge.lifetimeTestInvoke(UnsafeMutableRawPointer(bitPattern: bits)) == noErr)
            callbackDone.signal()
        }
        precondition(entered.wait(timeout: .now() + 5) == .success)
        DispatchQueue.global().async {
            precondition(bridge.stop() == nil)
            stopDone.signal()
        }
        precondition(halStopEntered.wait(timeout: .now() + 5) == .success)
        precondition(stopDone.wait(timeout: .now() + 0.05) == .timedOut)
        leave.signal()
        precondition(callbackDone.wait(timeout: .now() + 5) == .success)
        precondition(stopDone.wait(timeout: .now() + 5) == .success)
        bridge.lifetimeTestSetInsidePipeline(nil)
        let stopped = bridge.lifetimeTestSnapshot()
        precondition(stopped.generation == nil && stopped.bytes == 0 && stopped.registrations == 0)
        precondition(AudioBridge.lifetimeTestInvoke(UnsafeMutableRawPointer(bitPattern: bits)) == noErr)
        precondition(bridge.lifetimeTestSnapshot().accesses == stopped.accesses)
    }

    static func retirementLedgerRetainsARCResourceAcrossFailure() {
        final class Probe {}
        final class WeakBox {
            weak var value: Probe?
            init(_ value: Probe) { self.value = value }
        }
        var resource: Probe? = Probe()
        let observed = WeakBox(resource!)
        var ledger = AudioIOProcRetirementLedger<Probe>()
        ledger.markStarted(at: ledger.append(resource!))
        resource = nil
        precondition(ledger.retire(stop: { _ in -1 }, destroy: { _ in 0 }, release: { _ in }) != nil)
        precondition(observed.value != nil && ledger.callbacksMayRun)
        precondition(ledger.retire(stop: { _ in 0 }, destroy: { _ in -2 }, release: { _ in }) != nil)
        precondition(observed.value != nil && !ledger.callbacksMayRun)
        precondition(ledger.retire(stop: { _ in preconditionFailure("stop already acknowledged") }, destroy: { _ in 0 }, release: { _ in }) == nil)
        precondition(observed.value == nil && ledger.isEmpty)
    }
}
