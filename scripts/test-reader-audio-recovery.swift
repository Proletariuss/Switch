import Foundation
import AVFoundation

enum BoundedBackendLog {
    static func append(_ data: Data, to url: URL, maximumBytes: Int, archives: Int) throws {}
}

/// Only platform effects are synthetic. This object has no session, ticket,
/// playback or tap state machine; those are the production bridge's fields.
private final class PlatformRecorder {
    enum Fault: Error { case engineStart }
    var actions: [ReaderAudioRecoveryAction] = []
    var timers: [(TimeInterval, () -> Void)] = []
    var failedStarts = 0

    var environment: ReaderAudioRecoveryEnvironment {
        ReaderAudioRecoveryEnvironment(
            schedule: { [unowned self] delay, work in timers.append((delay, work)) },
            record: { [unowned self] action in actions.append(action) },
            startEngine: { [unowned self] in
                if failedStarts > 0 { failedStarts -= 1; throw Fault.engineStart }
            })
    }

    func fireNext() {
        precondition(!timers.isEmpty, "production closure was not scheduled")
        let (delay, work) = timers.removeFirst()
        precondition(delay == 0.4, "production retry deadline changed")
        work()
    }
}

private actor StartBarrier {
    private var entered = false
    private var entryWaiter: CheckedContinuation<Void, Never>?
    private var releaseWaiter: CheckedContinuation<Void, Never>?

    func pause() async {
        await withCheckedContinuation { continuation in
            releaseWaiter = continuation
            entered = true
            entryWaiter?.resume()
            entryWaiter = nil
        }
    }
    func waitForEntry() async {
        if entered { return }
        await withCheckedContinuation { entryWaiter = $0 }
    }
    func release() {
        precondition(entered && releaseWaiter != nil)
        releaseWaiter?.resume()
        releaseWaiter = nil
    }
}

@main private enum ReaderAudioRecoveryHarness {
    static func enqueue(_ count: Int, into bridge: ReaderVoWiFiAudioBridge) {
        let format = AVAudioFormat(standardFormatWithSampleRate: 16_000, channels: 1)!
        for index in 0..<count {
            let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 320)!
            buffer.frameLength = 320
            buffer.floatChannelData![0][0] = Float(index) / 10
            bridge.recoveryTestEnqueue(buffer)
        }
    }

    static func assertNoEffect(_ bridge: ReaderVoWiFiAudioBridge, _ recorder: PlatformRecorder,
                               snapshot: ReaderVoWiFiAudioBridge.RecoveryTestSnapshot,
                               actions: [ReaderAudioRecoveryAction]) {
        recorder.fireNext() // Executes the stored production retry closure.
        precondition(bridge.recoveryTestSnapshot() == snapshot,
            "old timer mutated production session/recovery/tap/playback state")
        precondition(recorder.actions == actions,
            "old timer touched engine, tap or playback actions")
        precondition(recorder.timers.isEmpty, "old timer scheduled more work")
    }

    static func oldTimerAfterNewSession(frameCount: Int) async {
        let recorder = PlatformRecorder()
        let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(recorder.environment)
        let startResult = await bridge.recoveryTestStartSession()
        precondition(startResult == nil)
        enqueue(3, into: bridge)
        recorder.failedStarts = 1
        bridge.recoveryTestRouteChange() // Real recovery fails and queues retry.
        precondition(recorder.timers.count == 1 && bridge.audioRouteError() != nil)
        let old = bridge.recoveryTestSnapshot()
        bridge.stop() // Real public stop retires session and tap.
        precondition(!bridge.recoveryTestSnapshot().running)
        do { let result = await bridge.recoveryTestStartSession(); precondition(result == nil) }
        enqueue(frameCount, into: bridge)
        let current = bridge.recoveryTestSnapshot()
        precondition(current.running && current.tapInstalled && current.session != old.session)
        if frameCount < 6 { precondition(current.pending.count == frameCount && current.queued == 0) }
        else { precondition(current.pending.isEmpty && current.queued == frameCount) }
        assertNoEffect(bridge, recorder, snapshot: current, actions: recorder.actions)

        // Positive control: the exact same production timer is allowed to
        // recover its current session, and effects must reach this recorder.
        recorder.failedStarts = 1
        bridge.recoveryTestRouteChange()
        precondition(recorder.timers.count == 1)
        let before = recorder.actions.count
        let beforeSnapshot = bridge.recoveryTestSnapshot()
        recorder.fireNext()
        let effects = Array(recorder.actions.dropFirst(before))
        for action in [ReaderAudioRecoveryAction.engineStop, .engineStart, .tapRemove, .tapInstall,
                       .playbackClear, .graphReconnect] {
            precondition(effects.contains(action), "live recovery control missed \(action)")
        }
        let recovered = bridge.recoveryTestSnapshot()
        precondition(recovered.session == beforeSnapshot.session && recovered.recovery != beforeSnapshot.recovery)
        precondition(recovered.tapGeneration != beforeSnapshot.tapGeneration && bridge.audioRouteError() == nil)
        bridge.stop()
    }

    static func oldTimerAfterSuccessfulRecovery() async {
        let recorder = PlatformRecorder()
        let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(recorder.environment)
        do { let result = await bridge.recoveryTestStartSession(); precondition(result == nil) }
        recorder.failedStarts = 1
        bridge.recoveryTestRouteChange()
        precondition(recorder.timers.count == 1)
        bridge.recoveryTestRouteChange() // Notification wins before old timer.
        precondition(bridge.audioRouteError() == nil)
        enqueue(4, into: bridge)
        assertNoEffect(bridge, recorder, snapshot: bridge.recoveryTestSnapshot(), actions: recorder.actions)
        bridge.stop()
    }

    static func oldTimerAfterStop() async {
        let recorder = PlatformRecorder()
        let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(recorder.environment)
        do { let result = await bridge.recoveryTestStartSession(); precondition(result == nil) }
        recorder.failedStarts = 1
        bridge.recoveryTestRouteChange()
        precondition(recorder.timers.count == 1)
        bridge.stop()
        assertNoEffect(bridge, recorder, snapshot: bridge.recoveryTestSnapshot(), actions: recorder.actions)
    }

    static func launch(_ bridge: ReaderVoWiFiAudioBridge, _ ticket: ReaderAudioSessionTicket,
                       capture: Bool = true) async -> String? {
        await bridge.start(port: 49999, token: "test", captureMicrophone: capture,
                           localRingback: false, session: ticket)
    }

    static func startupInterleavings() async {
        // Cancellation/hangup before the async start reaches its queue must
        // never adopt the generation that stop just created.
        do {
            let recorder = PlatformRecorder()
            let barrier = StartBarrier()
            var environment = recorder.environment
            environment.beforeStart = { await barrier.pause() }
            let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(environment)
            let ticket = ReaderAudioSessionTicket()
            let task = Task { await launch(bridge, ticket) }
            await barrier.waitForEntry()
            bridge.stop(session: ticket)
            task.cancel()
            let stopped = bridge.recoveryTestSnapshot()
            let actions = recorder.actions
            await barrier.release()
            let result = await task.value
            precondition(result != nil && bridge.recoveryTestSnapshot() == stopped)
            precondition(recorder.actions == actions && !stopped.resourcesInstalled)
        }
        // Both receive-only and duplex startup must clean graph/connection
        // resources if stop wins after installation and before engine startup.
        for capture in [false, true] {
            let recorder = PlatformRecorder()
            let barrier = StartBarrier()
            var environment = recorder.environment
            environment.afterInstall = { await barrier.pause() }
            let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(environment)
            let ticket = ReaderAudioSessionTicket()
            let task = Task { await launch(bridge, ticket, capture: capture) }
            await barrier.waitForEntry()
            precondition(bridge.recoveryTestSnapshot().resourcesInstalled)
            precondition(recorder.actions.contains(.graphInstall) && recorder.actions.contains(.connectionStart))
            bridge.stop(session: ticket)
            task.cancel()
            let stopped = bridge.recoveryTestSnapshot()
            let actions = recorder.actions
            await barrier.release()
            let result = await task.value
            precondition(result != nil && !stopped.running && !stopped.resourcesInstalled)
            precondition(bridge.recoveryTestSnapshot() == stopped && recorder.actions == actions)
            precondition(!recorder.actions.contains(.engineStart) && !recorder.actions.contains(.handshake))
        }
        // A suspended permission completion cannot install a tap after hangup,
        // including a denial: the completion must still be scoped to its call.
        for granted in [false, true] {
            let recorder = PlatformRecorder()
            let barrier = StartBarrier()
            var environment = recorder.environment
            environment.microphoneAccess = { await barrier.pause(); return granted }
            let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(environment)
            let ticket = ReaderAudioSessionTicket()
            let task = Task { await launch(bridge, ticket) }
            await barrier.waitForEntry()
            bridge.stop(session: ticket)
            let stopped = bridge.recoveryTestSnapshot()
            let actions = recorder.actions
            await barrier.release()
            let result = await task.value
            precondition(result != nil && bridge.recoveryTestSnapshot() == stopped)
            precondition(recorder.actions == actions && !recorder.actions.contains(.tapInstall))
        }
        // Cancel an old permission waiter only after a successor is running.
        // Its cancellation handler and late permission cleanup must leave the
        // successor's graph, tap, buffers and engine completely untouched.
        do {
            let recorder = PlatformRecorder()
            let barrier = StartBarrier()
            var environment = recorder.environment
            environment.microphoneAccess = { await barrier.pause(); return true }
            let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(environment)
            let oldTicket = ReaderAudioSessionTicket()
            let task = Task { await launch(bridge, oldTicket) }
            await barrier.waitForEntry()
            let nextTicket = ReaderAudioSessionTicket()
            let result = await launch(bridge, nextTicket, capture: false)
            precondition(result == nil)
            enqueue(3, into: bridge)
            let successor = bridge.recoveryTestSnapshot()
            let actions = recorder.actions
            task.cancel()
            await barrier.release()
            let oldResult = await task.value
            precondition(oldResult != nil && bridge.recoveryTestSnapshot() == successor)
            bridge.stop(session: oldTicket) // Same cleanup used by CallAudioStore.
            precondition(recorder.actions == actions && bridge.recoveryTestSnapshot() == successor)
            bridge.stop(session: nextTicket)
        }
        // Failing engine startup and denied microphone permission must retire
        // the exact installed ticket and leave no graph/connection/tap alive.
        for failEngine in [false, true] {
            let recorder = PlatformRecorder()
            recorder.failedStarts = failEngine ? 1 : 0
            var environment = recorder.environment
            environment.microphoneAccess = { failEngine }
            let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(environment)
            let result = await launch(bridge, ReaderAudioSessionTicket())
            let state = bridge.recoveryTestSnapshot()
            precondition(result != nil && !state.running && !state.resourcesInstalled && !state.tapInstalled)
            precondition(!recorder.actions.contains(.handshake))
        }
        // Positive control: real startup with platform effects replaced reaches
        // tap installation, engine startup, and authenticated handshake once.
        do {
            let recorder = PlatformRecorder()
            let bridge = ReaderVoWiFiAudioBridge.recoveryTestBridge(recorder.environment)
            let ticket = ReaderAudioSessionTicket()
            let result = await launch(bridge, ticket)
            let state = bridge.recoveryTestSnapshot()
            precondition(result == nil && state.running && state.tapInstalled && state.resourcesInstalled)
            for action in [ReaderAudioRecoveryAction.graphInstall, .connectionStart, .tapInstall, .engineStart, .handshake] {
                precondition(recorder.actions.filter { $0 == action }.count == 1)
            }
            bridge.stop(session: ticket)
            precondition(!bridge.recoveryTestSnapshot().resourcesInstalled)
        }
    }

    static func main() async {
        await oldTimerAfterNewSession(frameCount: 3) // Actual retained prime buffers.
        await oldTimerAfterNewSession(frameCount: 8) // Actual Rust scheduled queue.
        await oldTimerAfterSuccessfulRecovery()
        await oldTimerAfterStop()
        await startupInterleavings()
        print("PASS: 4 production stale-recovery scenarios, 2 live timer controls and 9 production startup/cancellation/failure scenarios (virtual platform; no devices or network)")
    }
}
