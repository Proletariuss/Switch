// Compile together with production CallAudioStore.swift and CallModels.swift.
// These adapters never create an audio engine, socket, API request or device.
import Foundation

@MainActor final class BackendProcess {
    enum State { case running, stopped }
    var state = State.running
    let logURL = URL(fileURLWithPath: ProcessInfo.processInfo.environment["CALL_AUDIO_STORE_TEST_ROOT"]!)
        .appendingPathComponent("unused-backend.log")
}

@MainActor final class AudioBridge {
    static let shared = AudioBridge()
    var onDefaultDeviceChange: (() -> Void)?
    var onStart: (() -> Void)?
    var permissionGranted = true
    var permissionRequests = 0
    var starts = 0
    var stops = 0
    var stopError: String?
    var platformRunning = false
    var retirementPending = false
    var lifecycleSnapshot: AudioIOProcLifecycleSnapshot {
        AudioIOProcLifecycleSnapshot(running: platformRunning, retirementPending: retirementPending)
    }
    var holdPermission = false
    private var permissionWaiters: [Int: CheckedContinuation<Bool, Never>] = [:]
    var waitingPermissions: [Int] { permissionWaiters.keys.sorted() }
    func requestMicrophoneAccess() async -> Bool {
        permissionRequests += 1
        let request = permissionRequests
        if holdPermission {
            return await withCheckedContinuation { permissionWaiters[request] = $0 }
        }
        return permissionGranted
    }
    func completePermission(_ request: Int, granted: Bool = true) {
        precondition(permissionWaiters[request] != nil)
        permissionWaiters.removeValue(forKey: request)!.resume(returning: granted)
    }
    func start() -> String? {
        guard !retirementPending else { return "prior IOProc retirement pending" }
        precondition(ReaderVoWiFiAudioBridge.shared.connected == nil, "overlapping module/reader owners")
        starts += 1
        platformRunning = true
        onStart?()
        return nil
    }
    func stop() -> String? {
        stops += 1
        retirementPending = stopError != nil
        if stopError == nil || stopError?.contains("Destroy") == true { platformRunning = false }
        return stopError
    }
    func reset() {
        precondition(permissionWaiters.isEmpty, "unconsumed module permission completion")
        onDefaultDeviceChange = nil
        onStart = nil
        permissionGranted = true
        holdPermission = false
        permissionRequests = 0
        starts = 0
        stops = 0
        stopError = nil
        platformRunning = false
        retirementPending = false
    }
}

final class ReaderAudioSessionTicket {}

@MainActor final class ReaderVoWiFiAudioBridge {
    static let shared = ReaderVoWiFiAudioBridge()
    struct Start {
        let id: Int
        let port: Int
        let token: String
        let capture: Bool
        let ringback: Bool
    }
    var starts: [Start] = []
    var enableCalls = 0
    var permissionRequests = 0
    var stops = 0
    var holdStarts = false
    var holdMicrophone = false
    var ringback = false
    var microphone = false
    var connected: Start?
    private var sessionTicket: ReaderAudioSessionTicket?
    private var generation = 0
    private var startWaiters: [Int: CheckedContinuation<String?, Never>] = [:]
    private var microphoneWaiters: [Int: CheckedContinuation<String?, Never>] = [:]
    var waitingStarts: [Int] { startWaiters.keys.sorted() }
    var waitingMicrophones: [Int] { microphoneWaiters.keys.sorted() }

    func start(port: Int, token: String, captureMicrophone: Bool, localRingback: Bool,
               sampleRate: Int, diagnosticURL: URL?, session: ReaderAudioSessionTicket) async -> String? {
        precondition(!AudioBridge.shared.platformRunning && !AudioBridge.shared.retirementPending,
                     "reader admitted before module retirement")
        precondition(sampleRate == 16_000)
        precondition(diagnosticURL != nil)
        sessionTicket = session
        generation += 1
        let expected = generation
        let call = Start(id: expected, port: port, token: token,
                         capture: captureMicrophone, ringback: localRingback)
        starts.append(call)
        connected = call
        microphone = false
        ringback = false
        let failure: String?
        if holdStarts {
            failure = await withCheckedContinuation { startWaiters[expected] = $0 }
        } else { failure = nil }
        // Model the production bridge's own session-generation fence as well
        // as the store's revision fence; a late start cannot own a new route.
        guard expected == generation else { return "fixture: retired bridge session" }
        if let failure { connected = nil; return failure }
        if captureMicrophone {
            permissionRequests += 1
            microphone = true
        }
        ringback = localRingback
        return nil
    }
    func enableMicrophone(session: ReaderAudioSessionTicket) async -> String? {
        guard sessionTicket === session else { return "fixture: retired ticket" }
        enableCalls += 1
        permissionRequests += 1
        let expected = generation
        let failure: String?
        if holdMicrophone {
            failure = await withCheckedContinuation { microphoneWaiters[expected] = $0 }
        } else { failure = nil }
        guard expected == generation, connected != nil else {
            return "fixture: microphone grant belongs to retired session"
        }
        if let failure { return failure }
        microphone = true
        return nil
    }
    func completeStart(_ id: Int, failure: String? = nil) {
        precondition(startWaiters[id] != nil)
        startWaiters.removeValue(forKey: id)!.resume(returning: failure)
    }
    func completeMicrophone(_ id: Int, failure: String? = nil) {
        precondition(microphoneWaiters[id] != nil)
        microphoneWaiters.removeValue(forKey: id)!.resume(returning: failure)
    }
    func stop(session: ReaderAudioSessionTicket) {
        guard sessionTicket === session else { return }
        stop()
    }
    func stop() {
        sessionTicket = nil
        generation += 1
        stops += 1
        connected = nil
        microphone = false
        ringback = false
    }
    func setLocalRingbackEnabled(_ enabled: Bool) { ringback = enabled }
    func audioProcessingDescription() -> String { "fixture processing" }
    func audioRoute() -> (input: String, output: String) { ("fixture input", "fixture output") }
    func audioRouteError() -> String? { nil }
    func reset() {
        precondition(startWaiters.isEmpty && microphoneWaiters.isEmpty, "unconsumed controlled completion")
        stop()
        starts = []
        enableCalls = 0
        permissionRequests = 0
        stops = 0
        holdStarts = false
        holdMicrophone = false
    }
}

@MainActor enum APITrace {
    static var paths: [String] = []
    static var heldPaths = Set<String>()
    static var waiters: [Int: CheckedContinuation<String, Never>] = [:]
    static var startResult = true
    static var startThrows = false
    static func complete(_ index: Int, json: String) {
        precondition(waiters[index] != nil)
        waiters.removeValue(forKey: index)!.resume(returning: json)
    }
}
@MainActor struct APIClient {
    init(timeoutInterval: TimeInterval = 20) {}
    func send<T: Decodable>(_ path: String) async throws -> T {
        APITrace.paths.append(path)
        if path == "api/call/audio/start", APITrace.startThrows {
            throw NSError(domain: "fixture deterministic start failure", code: 1)
        }
        let index = APITrace.paths.count - 1
        let json: String
        if APITrace.heldPaths.contains(path) {
            json = await withCheckedContinuation { APITrace.waiters[index] = $0 }
            return try JSONDecoder().decode(T.self, from: Data(json.utf8))
        }
        switch path {
        case "api/call/audio/start": json = "{\"started\":\(APITrace.startResult),\"runtime_version\":\"fixture\"}"
        case "api/call/audio/stop": json = #"{"stopped":true}"#
        default: preconditionFailure("unexpected mock API path: \(path)")
        }
        return try JSONDecoder().decode(T.self, from: Data(json.utf8))
    }
}

@MainActor private final class Fixture {
    let store: CallAudioStore
    let backend = BackendProcess()
    var endpoint = "reader"
    var moduleReady = true
    var status = CallStatus(state: "idle", number: nil, incoming: false, active: false)
    var reader: ReaderVoWiFiCallState?
    init() {
        AudioBridge.shared.reset()
        ReaderVoWiFiAudioBridge.shared.reset()
        APITrace.paths = []
        precondition(APITrace.waiters.isEmpty, "unconsumed backend completion")
        APITrace.heldPaths = []
        APITrace.startResult = true
        APITrace.startThrows = false
        store = CallAudioStore(backend: backend)
        store.currentEndpoint = { [weak self] in self?.endpoint ?? "module" }
        store.currentStatus = { [weak self] in self!.status }
        store.currentReaderState = { [weak self] in self?.reader }
        store.moduleCallModeReady = { [weak self] in self?.moduleReady ?? false }
    }
    func publish(_ display: String, raw: String? = nil, direction: String = "incoming",
                 ready: Bool = true, early: Bool = false, signaling: Bool = true, port: Int? = 5001,
                 token: String? = "fixture-1", callID: String? = "fixture-call") {
        let previous = status
        if let raw {
            reader = ReaderVoWiFiCallState(
                state: raw, direction: direction, peer: "fixture", callID: callID,
                codec: "AMR-WB", startedAt: nil, lastError: nil,
                mediaReady: ready, earlyMedia: early, signalingReady: signaling,
                audioPort: port, audioToken: token,
                rtpPacketsSent: nil, rtpPacketsReceived: nil, rtpDecodeErrors: nil, audioFramesToHost: nil)
        } else { reader = nil }
        // Explicit display/raw pairs are fixture inputs. Production audio
        // decisions are always executed by CallAudioStore, never copied here.
        status = CallStatus(state: display, number: "fixture", incoming: display == "incoming", active: display == "active")
        store.handleStatusChange(previous: previous, status: status)
    }
    func finish() { store.stop(stopModuleRoute: false) }
}

@MainActor private func settle() async { for _ in 0..<30 { await Task.yield() } }
@MainActor private func waitFor(_ message: String, _ predicate: () -> Bool) async {
    for _ in 0..<400 {
        if predicate() { return }
        try? await Task.sleep(nanoseconds: 5_000_000)
    }
    preconditionFailure(message)
}
@MainActor private func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
}

@main private enum AudioStoreTests {
    @MainActor static func main() async {
        audioIOProcRetirementLedgerRequiresAcknowledgedCleanup()
        audioRouteRecoveryRetryIsFencedByNewRecovery()
        await incomingReceiveOnlyPromotesOnce()
        await offerlessReceiveOnlyPromotesOnce()
        await offerlessAckDuringAttachmentPromotesOnce()
        await offerlessHangupFencesLateAckAndAttachment()
        await offerlessInvalidationFencesLateAttachment()
        await offerlessRouteRecoveryDoesNotCapture()
        await ackDuringAttachmentPromotesWithoutAnotherPoll()
        await cancelledAttachmentDoesNotRevive()
        await retiredCompletionDoesNotReplaceNewRoute()
        await lateMicrophoneGrantDoesNotRevive()
        await cancelledBeforeTaskEntryDoesNotAttach()
        await outgoingEarlyMediaIsPreserved()
        await outgoingEarlyRouteChangeDoesNotRestart()
        await receiveOnlyAdmissionRequiresAllFacts()
        readerAdmissionAdapterUsesDisplayedStateNotActiveHint()
        await moduleRouteIsPreserved()
        await staleModuleStartSurfacesUnconfirmedLocalRetirement()
        await moduleRouteRecoveryDoesNotRestartWhenIOProcRetirementFails()
        await moduleRetirementBlocksReaderUntilAcknowledged()
        await modulePendingDuringReaderAttachmentRetiresIt()
        await modulePendingDuringMicrophoneGrantRetiresReader()
        await readerToModuleReplacementRespectsPlatformRetirement()
        await liveModuleHandoffRetiresActualOwner()
        await liveModuleHandoffWaitsForBackendStartAndStop()
        await stoppedHandoffCannotResumeFromLateBackendStop()
        await backendStopRefusalKeepsReplacementFenced()
        await failedModuleActivationDoesNotLeaveLiveOwner()
        await backendUnavailableHandoffRecoversAfterRestart()
        await receiveOnlyRouteRecoveryDoesNotCapture()
        await pendingHangupFencesSameCallStatusUpdates()
        await sameCallIDChangingRouteDoesNotReleaseHangupFence()
        await pendingHangupFencesLateAttachment()
        await pendingHangupFencesLateMicrophoneGrant()
        await pendingHangupReleasesForNewCall()
        await pendingHangupReleasesAtTerminalOrReset()
        await pendingHangupUsesTokenWhenCallIDIsUnavailable()
        await moduleHangupAdmissionIsPreserved()
        await replacementIncomingRetiresOldCaptureWithoutIdle()
        await oldMicrophoneGrantCannotAlterReplacementPlayback()
        await moduleHangupFencesDelayedPermissionAndActivePolls()
        await modulePermissionChecksCurrentStateBeforeBackendStart()
        await retiredModulePermissionCannotAffectNewCall()
        await readerEndpointFallbackStopsCaptureBeforeUnknownModuleStatus()
        await readerEndpointFallbackDirectlyToActiveModuleStartsOnce()
        print("PASS: 43 async CallAudioStore scenarios, reader admission adapter, CoreAudio retirement ledger, and route-recovery retry fence; mock audio/API only")
    }

    static func audioIOProcRetirementLedgerRequiresAcknowledgedCleanup() {
        do {
            var ledger = AudioIOProcRetirementLedger<Int>()
            let index = ledger.append(7)
            ledger.markStarted(at: index)
            var events: [String] = []
            let failure = ledger.retire(
                stop: { events.append("stop:\($0)"); return -1 },
                destroy: { events.append("destroy:\($0)"); return 0 },
                release: { events.append("release:\($0)") })
            precondition(failure == AudioIOProcRetirementFailure(operation: .stop, status: -1))
            precondition(events == ["stop:7"] && ledger.entries.count == 1 && ledger.callbacksMayRun)
            events.removeAll()
            let retryFailure = ledger.retire(
                stop: { events.append("stop:\($0)"); return 0 },
                destroy: { events.append("destroy:\($0)"); return 0 },
                release: { events.append("release:\($0)") })
            precondition(retryFailure == nil && ledger.isEmpty && !ledger.callbacksMayRun)
            precondition(events == ["stop:7", "destroy:7", "release:7"])
        }

        do {
            var ledger = AudioIOProcRetirementLedger<Int>()
            let index = ledger.append(9)
            ledger.markStarted(at: index)
            var destroyAttempts = 0
            var releases = 0
            let failure = ledger.retire(
                stop: { _ in 0 },
                destroy: { _ in destroyAttempts += 1; return destroyAttempts == 1 ? -2 : 0 },
                release: { _ in releases += 1 })
            precondition(failure == AudioIOProcRetirementFailure(operation: .destroy, status: -2))
            precondition(ledger.entries.count == 1 && !ledger.callbacksMayRun && releases == 0)
            let retryFailure = ledger.retire(
                stop: { _ in preconditionFailure("an acknowledged stop must not be repeated") },
                destroy: { _ in destroyAttempts += 1; return 0 },
                release: { _ in releases += 1 })
            precondition(retryFailure == nil && ledger.isEmpty && destroyAttempts == 2 && releases == 1)
        }

        do {
            var ledger = AudioIOProcRetirementLedger<Int>()
            ledger.append(11) // created, but AudioDeviceStart never succeeded
            var events: [String] = []
            let failure = ledger.retire(
                stop: { _ in preconditionFailure("an unstarted IOProc must not be stopped") },
                destroy: { events.append("destroy:\($0)"); return 0 },
                release: { events.append("release:\($0)") })
            precondition(failure == nil && ledger.isEmpty)
            precondition(events == ["destroy:11", "release:11"])
        }
        print("PASS: CoreAudio IOProc ledger retains contexts until stop/destroy acknowledgement and retries only unfinished phases")
    }

    @MainActor static func audioRouteRecoveryRetryIsFencedByNewRecovery() {
        var tickets = AudioRouteRecoveryTicketLedger()
        tickets.retireSession()
        let oldRetry = tickets.scheduleRetry() // represents the pending 400 ms ticket

        // A route-change notification recovers successfully before the timer fires.
        tickets.claimRecovery()
        var engineRestartCount = 1 // the route-change recovery itself restarted it once

        // Fire the old timer deterministically, without sleeping or relying on wall time.
        if tickets.isCurrent(oldRetry, running: true) {
            engineRestartCount += 1
        }
        expect(engineRestartCount == 1, "stale 400 ms retry restarted the engine after route recovery")

        let retryAfterNewFailure = tickets.scheduleRetry()
        expect(tickets.isCurrent(retryAfterNewFailure, running: true), "new retry ticket was not current")
        tickets.retireSession()
        expect(!tickets.isCurrent(retryAfterNewFailure, running: true), "stopping the call did not retire a retry")
    }

    @MainActor static func incomingReceiveOnlyPromotesOnce() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("receive-only attachment", { f.store.running && !f.store.activationInFlight })
        expect(bridge.starts.count == 1 && !bridge.starts[0].capture && !bridge.starts[0].ringback, "incoming answer attached a microphone or local ringback")
        expect(!f.store.readerMicrophoneActive && bridge.permissionRequests == 0 && !bridge.microphone && !bridge.ringback, "incoming answer requested capture before ACK")
        let connection = bridge.connected!.id
        f.publish("active", raw: "active")
        await waitFor("active microphone promotion", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 1 && bridge.enableCalls == 1 && bridge.connected?.id == connection, "active restarted the PCM connection")
        for _ in 0..<3 { f.publish("active", raw: "active") }
        await settle()
        expect(bridge.starts.count == 1 && bridge.enableCalls == 1 && bridge.permissionRequests == 1, "duplicate ACK restarted capture")
        expect(APITrace.paths.isEmpty && AudioBridge.shared.permissionRequests == 0, "reader call touched module audio")
        print("PASS: incoming connecting RX-only -> active reuses one connection; duplicate ACK is idempotent")
    }
    @MainActor static func offerlessReceiveOnlyPromotesOnce() async {
        for endpoint in ["reader", "module-vowifi"] {
            let f = Fixture()
            f.endpoint = endpoint
            let bridge = ReaderVoWiFiAudioBridge.shared
            f.publish("dialing", raw: "connecting", ready: false, early: false)
            await waitFor("offerless receive-only attachment", { f.store.running && !f.store.activationInFlight })
            let connection = bridge.connected!.id
            expect(bridge.starts.count == 1 && !bridge.starts[0].capture && !bridge.starts[0].ringback,
                   "offerless preparation enabled capture or local ringback")
            for _ in 0..<3 { f.publish("dialing", raw: "connecting", ready: false, early: false) }
            await settle()
            expect(bridge.starts.count == 1 && bridge.permissionRequests == 0 && !bridge.microphone && !bridge.ringback,
                   "offerless connecting polls reopened audio or started capture/ringback")
            f.publish("active", raw: "active")
            await waitFor("offerless ACK microphone promotion", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
            for _ in 0..<3 { f.publish("active", raw: "active") }
            await settle()
            expect(bridge.connected?.id == connection && bridge.starts.count == 1
                && bridge.enableCalls == 1 && bridge.permissionRequests == 1,
                   "offerless ACK replaced the prepared PCM connection or repeated permission")
            expect(APITrace.paths.isEmpty && AudioBridge.shared.permissionRequests == 0,
                   "offerless reader path touched module audio")
            f.finish()
        }
        print("PASS: offerless incoming with ready=false/early=false attaches RX-only on both endpoints, then ACK promotes once")
    }
    @MainActor static func offerlessAckDuringAttachmentPromotesOnce() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        bridge.holdStarts = true
        f.publish("dialing", raw: "connecting", ready: false, early: false)
        await waitFor("offerless attachment held", { bridge.waitingStarts.count == 1 })
        let connection = bridge.waitingStarts[0]
        f.publish("active", raw: "active")
        await settle()
        expect(bridge.permissionRequests == 0 && bridge.enableCalls == 0, "ACK captured before PCM attachment completed")
        bridge.completeStart(connection)
        await waitFor("offerless attach completion promotes pending ACK", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 1 && bridge.connected?.id == connection && bridge.enableCalls == 1 && bridge.permissionRequests == 1,
               "offerless in-flight ACK restarted attachment or needed another poll")
        print("PASS: ACK during offerless RX attachment promotes the same connection without a later poll")
    }
    @MainActor static func offerlessHangupFencesLateAckAndAttachment() async {
        for pending in [false, true] {
            let f = Fixture()
            let bridge = ReaderVoWiFiAudioBridge.shared
            bridge.holdStarts = pending
            f.publish("dialing", raw: "connecting", ready: false, early: false)
            await waitFor("offerless pre-hangup attach", {
                pending ? bridge.waitingStarts.count == 1 : f.store.running && !f.store.activationInFlight
            })
            let attachment = bridge.waitingStarts.first
            f.store.prepareForHangup(endpoint: "reader")
            for _ in 0..<3 {
                f.publish("active", raw: "active")
                f.publish("dialing", raw: "connecting", ready: false, early: false)
            }
            if let attachment { bridge.completeStart(attachment) }
            await settle()
            f.publish("active", raw: "active")
            AudioBridge.shared.onDefaultDeviceChange?()
            await settle()
            expect(bridge.starts.count == 1 && bridge.enableCalls == 0 && bridge.permissionRequests == 0,
                   "late offerless ACK/poll bypassed hangup intent")
            expect(!f.store.running && !f.store.activationInFlight && !f.store.readerMicrophoneActive
                && bridge.connected == nil && !bridge.microphone,
                   "retired offerless attachment revived after hangup")
            f.finish()
        }
        print("PASS: offerless pending hangup fences late ACK, repeated connecting polls and delayed attachment")
    }
    @MainActor static func offerlessInvalidationFencesLateAttachment() async {
        for invalidation in ["failed", "unknown", "unknown-stale-connecting", "missing-endpoint", "missing-port", "missing-token", "empty-token", "signaling-stopped"] {
            let f = Fixture()
            let bridge = ReaderVoWiFiAudioBridge.shared
            bridge.holdStarts = true
            f.publish("dialing", raw: "connecting", ready: false, early: false)
            await waitFor("offerless attach before invalidation", { bridge.waitingStarts.count == 1 })
            let attachment = bridge.waitingStarts[0]
            switch invalidation {
            case "failed": f.publish("idle", raw: "failed", ready: false)
            case "unknown": f.publish("unknown", raw: "unknown", ready: false)
            case "unknown-stale-connecting": f.publish("unknown", raw: "connecting", ready: false)
            case "missing-endpoint": f.endpoint = "unknown"; f.publish("dialing", raw: "connecting", ready: false)
            case "missing-port": f.publish("dialing", raw: "connecting", ready: false, port: nil)
            case "missing-token": f.publish("dialing", raw: "connecting", ready: false, token: nil)
            case "empty-token": f.publish("dialing", raw: "connecting", ready: false, token: "")
            default: f.publish("dialing", raw: "connecting", ready: false, signaling: false)
            }
            bridge.completeStart(attachment)
            AudioBridge.shared.onDefaultDeviceChange?()
            await settle()
            expect(bridge.starts.count == 1 && bridge.enableCalls == 0 && bridge.permissionRequests == 0,
                   "invalidated offerless attachment started new audio/capture")
            expect(!f.store.running && !f.store.activationInFlight && !f.store.readerMicrophoneActive
                && bridge.connected == nil && !bridge.microphone,
                   "invalidated offerless completion revived its route")
            f.finish()
        }
        print("PASS: failed/unknown, lost endpoint/port/token and signaling loss fence late offerless attachments")
    }
    @MainActor static func offerlessRouteRecoveryDoesNotCapture() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", ready: false, early: false)
        await waitFor("offerless playback before route recovery", { f.store.running && !f.store.activationInFlight })
        AudioBridge.shared.onDefaultDeviceChange?()
        await waitFor("offerless playback route recovered", { bridge.starts.count == 2 && f.store.running && !f.store.activationInFlight })
        expect(bridge.starts.allSatisfy { !$0.capture && !$0.ringback } && bridge.permissionRequests == 0,
               "offerless route recovery requested capture or ringback")
        let recovered = bridge.connected!.id
        f.publish("active", raw: "active")
        await waitFor("offerless recovered route promoted", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.connected?.id == recovered && bridge.starts.count == 2 && bridge.enableCalls == 1,
               "offerless ACK replaced the recovered PCM connection")
        print("PASS: offerless route recovery stays RX-only until ACK and reuses its recovered connection")
    }
    @MainActor static func ackDuringAttachmentPromotesWithoutAnotherPoll() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared; bridge.holdStarts = true
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("controlled attach began", { bridge.waitingStarts.count == 1 })
        let connection = bridge.waitingStarts[0]
        f.publish("active", raw: "active")
        await settle()
        expect(bridge.enableCalls == 0 && f.store.activationInFlight, "microphone enabled before attachment completed")
        bridge.completeStart(connection)
        await waitFor("ACK promoted from attachment completion", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 1 && bridge.enableCalls == 1 && bridge.connected?.id == connection, "in-flight ACK needed another poll or a new connection")
        print("PASS: ACK during pending attachment promotes on completion without another status event")
    }
    @MainActor static func cancelledAttachmentDoesNotRevive() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared; bridge.holdStarts = true
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("pending attach", { bridge.waitingStarts.count == 1 })
        let old = bridge.waitingStarts[0]
        f.publish("idle", raw: "ended", ready: false)
        expect(!f.store.running && !f.store.activationInFlight && bridge.connected == nil, "hangup retained audio")
        bridge.completeStart(old)
        await settle()
        expect(!f.store.running && !f.store.readerMicrophoneActive && !f.store.activationInFlight && bridge.connected == nil && !bridge.microphone, "late attach revived ended call")
        expect(f.store.readerPhase == "未连接", "late attach rewrote the terminal phase")
        print("PASS: hangup fences a late attachment completion")
    }
    @MainActor static func retiredCompletionDoesNotReplaceNewRoute() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared; bridge.holdStarts = true
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("old attach", { bridge.waitingStarts.count == 1 })
        let old = bridge.waitingStarts[0]
        f.publish("idle", raw: "ended", ready: false)
        f.publish("dialing", raw: "connecting", early: true, port: 5002, token: "replacement")
        await waitFor("replacement attach", { bridge.waitingStarts.count == 2 })
        let fresh = bridge.waitingStarts.first { $0 != old }!
        bridge.completeStart(fresh)
        await waitFor("replacement ready", { f.store.running && !f.store.activationInFlight })
        bridge.completeStart(old)
        await settle()
        expect(f.store.running && !f.store.readerMicrophoneActive && bridge.connected?.token == "replacement", "old completion stopped or replaced the new connection")
        expect(bridge.starts.count == 2 && bridge.permissionRequests == 0, "replacement triggered premature capture")
        print("PASS: retired completion cannot overwrite a replacement token/port")
    }
    @MainActor static func lateMicrophoneGrantDoesNotRevive() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("playback ready", { f.store.running && !f.store.activationInFlight })
        bridge.holdMicrophone = true
        f.publish("active", raw: "active")
        await waitFor("microphone permission suspended", { bridge.waitingMicrophones.count == 1 })
        let grant = bridge.waitingMicrophones[0]
        f.publish("idle", raw: "ended", ready: false)
        bridge.completeMicrophone(grant)
        await settle()
        expect(!f.store.running && !f.store.readerMicrophoneActive && !f.store.activationInFlight && !bridge.microphone && bridge.connected == nil, "late microphone grant revived hangup")
        print("PASS: late microphone permission after hangup cannot reactivate capture")
    }
    @MainActor static func cancelledBeforeTaskEntryDoesNotAttach() async {
        let f = Fixture(); defer { f.finish() }
        f.publish("dialing", raw: "connecting", early: true)
        f.publish("idle", raw: "ended", ready: false)
        await settle()
        expect(ReaderVoWiFiAudioBridge.shared.starts.isEmpty && !f.store.running && !f.store.activationInFlight, "cancelled queued activation attached audio")
        print("PASS: cancellation before Task entry prevents bridge attachment")
    }
    @MainActor static func outgoingEarlyMediaIsPreserved() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("alerting", raw: "early", direction: "outgoing", ready: false, early: false)
        await waitFor("local outgoing ringback", { f.store.running && !f.store.activationInFlight })
        expect(bridge.starts.count == 1 && !bridge.starts[0].capture && bridge.starts[0].ringback && bridge.ringback, "outgoing local ringback changed")
        f.publish("alerting", raw: "early", direction: "outgoing", early: true)
        expect(!bridge.ringback && bridge.enableCalls == 0 && bridge.starts.count == 1, "remote early media enabled microphone or reattached")
        f.publish("active", raw: "active", direction: "outgoing")
        await waitFor("outgoing microphone", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 1 && bridge.enableCalls == 1, "outgoing answer no longer promotes existing audio")
        print("PASS: outgoing local ringback -> remote early media -> active behavior preserved")
    }
    @MainActor static func outgoingEarlyRouteChangeDoesNotRestart() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "early", direction: "outgoing", ready: false, early: false)
        await waitFor("outgoing ringback before route change", { f.store.running && !f.store.activationInFlight })
        let connection = bridge.connected!.id
        AudioBridge.shared.onDefaultDeviceChange?()
        try? await Task.sleep(for: .milliseconds(450)) // recovery debounce is 350 ms
        expect(bridge.starts.count == 1 && bridge.connected?.id == connection && bridge.stops == 0,
               "default device change restarted outgoing early/ringback audio")
        expect(bridge.ringback && !bridge.microphone && bridge.permissionRequests == 0,
               "outgoing early route change altered ringback or microphone admission")
        print("PASS: outgoing early media/ringback does not restart on default audio route change")
    }
    @MainActor static func readerAdmissionAdapterUsesDisplayedStateNotActiveHint() {
        let f = Fixture(); defer { f.finish() }
        f.publish("unknown", raw: "active")
        let displayedActive = CallStatus(state: "active", number: nil, incoming: nil, active: false)
        expect(ReaderAudioAdmission.decide(status: displayedActive, call: f.reader) == .duplex,
               "optional active hint overrode displayed state")
        expect(ReaderAudioAdmission.decide(status: nil, call: f.reader) == .closed,
               "missing displayed status did not fail closed")
        print("PASS: Swift reader admission snapshot follows displayed state and fails closed on missing status")
    }
    @MainActor static func receiveOnlyAdmissionRequiresAllFacts() async {
        let f = Fixture(); defer { f.finish() }
        f.publish("dialing", raw: "connecting", ready: false, port: nil)
        f.publish("dialing", raw: "connecting", ready: false, port: 0)
        f.publish("dialing", raw: "connecting", ready: false, port: 65536)
        f.publish("dialing", raw: "connecting", ready: false, token: nil)
        f.publish("dialing", raw: "connecting", ready: false, token: "")
        f.publish("dialing", raw: "connecting", ready: false, signaling: false)
        f.publish("unknown", raw: "connecting", ready: false)
        f.publish("dialing", raw: "failed", ready: false)
        f.publish("active", raw: "connecting")
        f.publish("dialing", raw: "connecting", direction: "outgoing", early: true)
        await settle()
        expect(ReaderVoWiFiAudioBridge.shared.starts.isEmpty, "incomplete or outgoing connecting state attached incoming playback")
        expect(ReaderVoWiFiAudioBridge.shared.permissionRequests == 0, "invalid incoming metadata requested capture")
        print("PASS: receive-only admission requires matching live state, incoming direction, signaling and valid port/token")
    }
    @MainActor static func moduleRouteIsPreserved() async {
        let f = Fixture(); defer { f.finish() }
        f.endpoint = "module"
        f.publish("active")
        await waitFor("module audio started", { f.store.running && !f.store.activationInFlight })
        expect(AudioBridge.shared.starts == 1 && AudioBridge.shared.permissionRequests == 1 && APITrace.paths == ["api/call/audio/start"], "module activation contract changed")
        f.publish("active")
        await settle()
        expect(AudioBridge.shared.starts == 1 && ReaderVoWiFiAudioBridge.shared.starts.isEmpty, "duplicate module status restarted a route")
        f.publish("idle")
        await waitFor("module stop acknowledged", { APITrace.paths.last == "api/call/audio/stop" })
        expect(!f.store.running && APITrace.paths.count == 2, "module hangup cleanup changed")
        f.moduleReady = false
        f.publish("active")
        await settle()
        expect(!f.store.running && APITrace.paths.count == 2 && f.store.error != nil, "unready module bypassed admission")
        print("PASS: module start/stop, duplicate status and call-mode admission preserved")
    }
    @MainActor static func moduleRouteRecoveryDoesNotRestartWhenIOProcRetirementFails() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = AudioBridge.shared
        f.endpoint = "module"
        f.publish("active")
        await waitFor("module route before retirement failure", { f.store.running && !f.store.activationInFlight })
        bridge.stopError = "AudioDeviceStop failed (OSStatus -1); callback context retained"
        bridge.onDefaultDeviceChange?()
        await waitFor("retirement failure surfaced", { f.store.running && f.store.error != nil })
        expect(bridge.starts == 1 && bridge.stops == 1,
               "route recovery restarted while old IOProc retirement was unconfirmed")
        expect(f.store.error?.contains("callback context retained") == true,
               "IOProc retirement failure was not visible to the call UI")
        expect(APITrace.paths == ["api/call/audio/start"],
               "failed local retirement issued an unexpected backend route operation")
        bridge.stopError = nil
        print("PASS: module route recovery stays fenced and reports an unconfirmed IOProc retirement")
    }

    @MainActor static func moduleRetirementBlocksReaderUntilAcknowledged() async {
        for stopFailed in [true, false] {
            let f = Fixture(); defer { f.finish() }
            let module = AudioBridge.shared
            let reader = ReaderVoWiFiAudioBridge.shared
            f.endpoint = "module"
            f.publish("active")
            await waitFor("module active before cross-route failure", { f.store.running && !f.store.activationInFlight })
            module.stopError = stopFailed ? "AudioDeviceStop failed" : "AudioDeviceDestroyIOProcID failed"
            f.store.stop(stopModuleRoute: false)
            expect(f.store.running == stopFailed && module.retirementPending,
                   "UI did not project the platform's running/pending snapshot")
            f.endpoint = "reader"
            f.store.running = false // A writable UI flag cannot erase authority.
            f.publish("active", raw: "active")
            await settle()
            expect(reader.starts.isEmpty && reader.permissionRequests == 0,
                   "reader replaced unretired module IOProcs")
            expect(f.store.running == stopFailed && f.store.error?.contains("未确认") == true,
                   "pending retirement state or its error was overwritten")
            f.store.reset()
            expect(module.retirementPending && f.store.error?.contains("未确认") == true,
                   "reset erased an unconfirmed platform retirement")
            module.stopError = nil
            f.store.stop(stopModuleRoute: false)
            f.publish("active", raw: "active")
            await waitFor("reader after confirmed module retirement", { f.store.running && !f.store.activationInFlight })
            expect(reader.starts.count == 1 && !module.retirementPending,
                   "confirmed cleanup failed to release reader admission")
        }
        print("PASS: module stop/destroy failures fence reader startup despite UI/reset changes; acknowledged cleanup releases admission")
    }

    @MainActor static func liveModuleHandoffRetiresActualOwner() async {
        for endpoint in ["reader", "module-vowifi"] {
            for failure in [nil, "AudioDeviceStop failed", "AudioDeviceDestroyIOProcID failed"] as [String?] {
                let f = Fixture(); defer { f.finish() }
                let module = AudioBridge.shared
                let reader = ReaderVoWiFiAudioBridge.shared
                f.endpoint = "module"
                f.publish("active")
                await waitFor("live module before handoff", { f.store.running && !f.store.activationInFlight })
                // No manual stop/idle. The scheduled old notification must not
                // recover a module graph after the signalling endpoint changes.
                module.onDefaultDeviceChange?()
                module.stopError = failure
                f.endpoint = endpoint
                f.store.running = false // Published UI state is not ownership.
                f.publish("active", raw: "active")
                module.onDefaultDeviceChange?()
                await settle()
                expect(module.stops > 0, "live module owner was not retired on endpoint change")
                if failure != nil {
                    expect(reader.starts.isEmpty && reader.permissionRequests == 0,
                           "failed live retirement admitted a Reader effect")
                    expect(module.retirementPending && f.store.error?.contains("未确认") == true,
                           "live handoff lost retirement authority/error")
                    try? await Task.sleep(for: .milliseconds(400))
                    expect(reader.starts.isEmpty, "default-device recovery bypassed retirement")
                    module.stopError = nil
                    f.publish("active", raw: "active")
                }
                await waitFor("reader after live module retirement", { reader.connected != nil && !f.store.activationInFlight })
                expect(!module.platformRunning && !module.retirementPending && module.starts == 1,
                       "live module and Reader platform owners overlapped")
                let count = reader.starts.count
                f.publish("active", raw: "active")
                await settle()
                expect(reader.starts.count == count, "duplicate live handoff restarted Reader")
                module.onDefaultDeviceChange?()
                await waitFor("Reader-only device recovery", { reader.starts.count > count && !f.store.activationInFlight })
                expect(!module.platformRunning && module.starts == 1, "notification revived old HAL owner")
                f.endpoint = "module"
                f.publish("active")
                await waitFor("normal reverse live switch", { module.platformRunning && !f.store.activationInFlight })
                expect(reader.connected == nil && !reader.microphone, "reverse switch retained Reader owner")
            }
        }
        print("PASS: live module→reader/module-vowifi retires HAL without manual stop; Stop/Destroy failures and device notifications never admit two owners; reverse switch works")
    }

    @MainActor static func liveModuleHandoffWaitsForBackendStartAndStop() async {
        let f = Fixture(); defer { f.finish() }
        APITrace.heldPaths = ["api/call/audio/start", "api/call/audio/stop"]
        f.endpoint = "module"
        f.publish("active")
        await waitFor("held old module backend start", { APITrace.waiters[0] != nil })
        f.endpoint = "reader"
        f.publish("active", raw: "active")
        await settle()
        expect(APITrace.paths.count == 1 && ReaderVoWiFiAudioBridge.shared.starts.isEmpty,
               "handoff did not wait for captured backend start")
        APITrace.complete(0, json: #"{"started":true,"runtime_version":"fixture"}"#)
        await waitFor("captured old backend stop", { APITrace.waiters[1] != nil })
        expect(AudioBridge.shared.starts == 0 && ReaderVoWiFiAudioBridge.shared.starts.isEmpty,
               "late old start or pending stop admitted a platform owner")
        AudioBridge.shared.onDefaultDeviceChange?()
        APITrace.complete(1, json: #"{"stopped":true}"#)
        await waitFor("Reader after captured backend retirement", { f.store.running && !f.store.activationInFlight })
        expect(APITrace.paths == ["api/call/audio/start", "api/call/audio/stop"],
               "late module completion submitted an unscoped extra stop")
        print("PASS: live handoff waits for captured backend start then stop; late module response never starts HAL or stops Reader")
    }

    @MainActor static func stoppedHandoffCannotResumeFromLateBackendStop() async {
        for next in ["stop", "hangup", "module", "reader"] {
            let f = Fixture(); defer { f.finish() }
            f.endpoint = "module"; f.publish("active")
            await waitFor("module before held stop", { f.store.running && !f.store.activationInFlight })
            APITrace.heldPaths = ["api/call/audio/stop"]
            f.endpoint = "module-vowifi"; f.publish("active", raw: "active")
            await waitFor("backend stop held", { APITrace.waiters[1] != nil })
            if next == "stop" {
                f.store.stop(stopModuleRoute: false)
            } else if next == "hangup" {
                f.store.prepareForHangup(endpoint: "module-vowifi")
                f.publish("active", raw: "active")
            } else {
                f.endpoint = next
                if next == "module" { f.publish("active") }
                else { f.publish("active", raw: "active", token: "replacement-token", callID: "replacement-call") }
            }
            APITrace.complete(1, json: #"{"stopped":true}"#)
            if next == "stop" || next == "hangup" {
                await settle()
                expect(ReaderVoWiFiAudioBridge.shared.starts.isEmpty && !f.store.running,
                       "late backend stop bypassed newer stop/hangup revision")
            } else if next == "module" {
                await waitFor("current module after old retirement", { AudioBridge.shared.starts == 2 && !f.store.activationInFlight })
                expect(ReaderVoWiFiAudioBridge.shared.starts.isEmpty, "old handoff resumed obsolete Reader intent")
            } else {
                await waitFor("current Reader identity after old retirement", { f.store.running && !f.store.activationInFlight })
                expect(ReaderVoWiFiAudioBridge.shared.connected?.token == "replacement-token",
                       "old handoff resumed obsolete Reader identity")
            }
        }
        print("PASS: late backend stop respects newer stop/hangup revision and re-evaluates changed endpoint/call identity")
    }

    @MainActor static func backendStopRefusalKeepsReplacementFenced() async {
        let f = Fixture(); defer { f.finish() }
        f.endpoint = "module"; f.publish("active")
        await waitFor("module before backend refusal", { f.store.running && !f.store.activationInFlight })
        APITrace.heldPaths = ["api/call/audio/stop"]
        f.endpoint = "reader"; f.publish("active", raw: "active")
        await waitFor("backend refusal held", { APITrace.waiters[1] != nil })
        APITrace.complete(1, json: #"{"stopped":false}"#)
        await settle()
        expect(ReaderVoWiFiAudioBridge.shared.starts.isEmpty && f.store.error?.contains("未确认") == true,
               "backend refusal did not fence replacement")
        f.publish("active", raw: "active")
        await waitFor("retry same old backend stop", { APITrace.waiters[2] != nil })
        APITrace.complete(2, json: #"{"stopped":true}"#)
        await waitFor("replacement after backend acknowledgement", { f.store.running && !f.store.activationInFlight })
        print("PASS: backend stop refusal retains owner fence until acknowledged retry")
    }

    @MainActor static func failedModuleActivationDoesNotLeaveLiveOwner() async {
        for failure in ["permission", "refused", "throw"] {
            let f = Fixture(); defer { f.finish() }
            AudioBridge.shared.permissionGranted = failure != "permission"
            APITrace.startResult = failure != "refused"
            APITrace.startThrows = failure == "throw"
            f.endpoint = "module"; f.publish("active")
            await waitFor("failed module activation", { !f.store.activationInFlight && f.store.error != nil })
            await settle()
            let paths = APITrace.paths.count
            AudioBridge.shared.onDefaultDeviceChange?()
            try? await Task.sleep(for: .milliseconds(400))
            expect(AudioBridge.shared.starts == 0 && APITrace.paths.count == paths,
                   "failed activation left a ghost live owner for device recovery")
            f.endpoint = "reader"; f.publish("active", raw: "active")
            await waitFor("Reader after failed module activation", { f.store.running && !f.store.activationInFlight })
            expect(!AudioBridge.shared.platformRunning, "failed module activation blocked clean replacement")
        }
        print("PASS: permission denial, started=false and deterministic start error leave no ghost live owner or device-triggered HAL start")
    }

    @MainActor static func backendUnavailableHandoffRecoversAfterRestart() async {
        let f = Fixture(); defer { f.finish() }
        f.endpoint = "module"; f.publish("active")
        await waitFor("module before backend unavailable", { f.store.running && !f.store.activationInFlight })
        f.backend.state = .stopped
        f.endpoint = "reader"; f.publish("active", raw: "active")
        await settle()
        expect(ReaderVoWiFiAudioBridge.shared.starts.isEmpty && !AudioBridge.shared.platformRunning
               && f.store.error?.contains("后端未运行") == true,
               "unavailable backend admitted replacement or hid the recovery condition")
        f.store.reset()
        expect(f.store.error?.contains("后端未运行") == true,
               "reset erased the backend retirement failure")
        f.backend.state = .running
        f.publish("active", raw: "active")
        await waitFor("handoff after backend restart", { f.store.running && !f.store.activationInFlight })
        expect(APITrace.paths == ["api/call/audio/start", "api/call/audio/stop"],
               "backend restart did not settle the captured original route")
        print("PASS: backend unavailable fails closed explicitly; a post-restart status retry settles old route and admits Reader")
    }

    @MainActor static func modulePendingDuringReaderAttachmentRetiresIt() async {
        for stopFailed in [true, false] {
            let f = Fixture(); defer { f.finish() }
            let reader = ReaderVoWiFiAudioBridge.shared
            reader.holdStarts = true
            f.publish("dialing", raw: "connecting", early: true)
            await waitFor("reader attachment held at platform await", { reader.waitingStarts.count == 1 })
            let waiting = reader.waitingStarts[0]
            let module = AudioBridge.shared
            module.platformRunning = stopFailed
            module.retirementPending = true
            reader.completeStart(waiting)
            await waitFor("pending module cancels late reader attachment", { !f.store.activationInFlight })
            expect(reader.connected == nil && !reader.microphone && f.store.running == stopFailed,
                   "late reader attachment bypassed the post-await module snapshot fence")
            expect(f.store.error?.contains("未确认") == true,
                   "late attachment hid unconfirmed platform retirement")
            module.stopError = nil
        }
        print("PASS: module pending during reader attachment retires the captured reader session before publication")
    }

    @MainActor static func modulePendingDuringMicrophoneGrantRetiresReader() async {
        let f = Fixture(); defer { f.finish() }
        let reader = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("reader playback before held microphone", { f.store.running && !f.store.activationInFlight })
        reader.holdMicrophone = true
        f.publish("active", raw: "active")
        await waitFor("held microphone grant", { reader.waitingMicrophones.count == 1 })
        AudioBridge.shared.retirementPending = true
        reader.completeMicrophone(reader.waitingMicrophones[0])
        await waitFor("pending module rejects microphone completion", { !f.store.activationInFlight })
        expect(reader.connected == nil && !reader.microphone && !f.store.readerMicrophoneActive,
               "late microphone completion crossed platform retirement fence")
        expect(f.store.error?.contains("未确认") == true,
               "late microphone completion erased retirement error")
        print("PASS: pending module retirement fences late reader microphone promotion")
    }

    @MainActor static func readerToModuleReplacementRespectsPlatformRetirement() async {
        let f = Fixture(); defer { f.finish() }
        let module = AudioBridge.shared
        let reader = ReaderVoWiFiAudioBridge.shared
        f.publish("active", raw: "active")
        await waitFor("reader active before module replacement", { f.store.running && !f.store.activationInFlight })
        module.stopError = "AudioDeviceDestroyIOProcID failed"
        f.endpoint = "module"
        f.publish("active")
        await settle()
        expect(reader.connected == nil && !reader.microphone && module.starts == 0,
               "module replacement started while HAL retirement was pending")
        expect(f.store.error?.contains("未确认") == true,
               "reader-to-module replacement lost retirement failure")
        module.stopError = nil
        f.store.stop(stopModuleRoute: false)
        f.store.startModuleAudio()
        await waitFor("module after confirmed retirement", { f.store.running && !f.store.activationInFlight })
        expect(module.starts == 1, "clean retirement did not release module admission")
        print("PASS: reader-to-module replacement stops old reader and waits for platform retirement acknowledgement")
    }
    @MainActor static func staleModuleStartSurfacesUnconfirmedLocalRetirement() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = AudioBridge.shared
        f.endpoint = "module"
        bridge.onStart = {
            bridge.onStart = nil
            f.store.stop()
            bridge.stopError = "AudioDeviceDestroyIOProcID failed (OSStatus -7); callback context retained"
        }
        f.publish("active")
        await waitFor("stale module start retirement error", {
            f.store.error?.contains("本机模块音频清理未确认") == true
        })
        expect(!f.store.running && bridge.starts == 1 && bridge.stops == 2,
               "stale module start did not retire its local route exactly through the cleanup fence")
        expect(f.store.error?.contains("callback context retained") == true,
               "stale module start discarded the local CoreAudio retirement failure")
        print("PASS: stale module start surfaces local IOProc retirement failure")
    }
    @MainActor static func receiveOnlyRouteRecoveryDoesNotCapture() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("receive-only route", { f.store.running && !f.store.activationInFlight })
        AudioBridge.shared.onDefaultDeviceChange?()
        await waitFor("receive-only route recovered", { bridge.starts.count == 2 && f.store.running && !f.store.activationInFlight })
        expect(bridge.starts.allSatisfy { !$0.capture && !$0.ringback } && bridge.permissionRequests == 0 && !bridge.microphone, "route recovery enabled microphone before ACK")
        let recovered = bridge.connected!.id
        f.publish("active", raw: "active")
        await waitFor("recovered route promoted", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 2 && bridge.connected?.id == recovered && bridge.enableCalls == 1, "active replaced the recovered PCM connection")
        print("PASS: incoming receive-only route recovery preserves playback-only policy")
    }
    @MainActor static func pendingHangupFencesSameCallStatusUpdates() async {
        for endpoint in ["reader", "module-vowifi"] {
            for initial in ["connecting", "early", "active"] {
                let f = Fixture()
                let bridge = ReaderVoWiFiAudioBridge.shared
                f.endpoint = endpoint
                f.publish(initial == "active" ? "active" : "dialing", raw: initial, early: true)
                await waitFor("initial audio before hangup", { f.store.running && !f.store.activationInFlight })
                f.store.prepareForHangup(endpoint: endpoint)
                let starts = bridge.starts.count
                let permissions = bridge.permissionRequests
                for _ in 0..<3 {
                    f.publish("active", raw: "active") // ACK/poll while API is pending.
                    f.publish("dialing", raw: "connecting", early: true)
                    f.publish("alerting", raw: "early", direction: "outgoing", early: false)
                }
                // Missing metadata or a changed route on the same call is
                // insufficient evidence to lift the hangup decision.
                f.publish("active", raw: "active", token: nil, callID: nil)
                f.publish("active", raw: "active", port: 5002, token: "same-call-new-route")
                AudioBridge.shared.onDefaultDeviceChange?()
                await settle()
                expect(bridge.starts.count == starts && bridge.permissionRequests == permissions && bridge.enableCalls == 0,
                       "same-call status update bypassed pending hangup")
                expect(!f.store.running && !f.store.readerMicrophoneActive && !f.store.activationInFlight && !bridge.microphone,
                       "pending hangup revived reader audio")
                f.finish()
            }
        }
        print("PASS: pending hangup suppresses active/ACK, connecting and early polls on both VoWiFi endpoints")
    }
    @MainActor static func sameCallIDChangingRouteDoesNotReleaseHangupFence() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("active", raw: "active", port: 5001, token: "original", callID: "same-call")
        await waitFor("same-call audio before hangup", { f.store.running && !f.store.activationInFlight })
        f.store.prepareForHangup(endpoint: "reader")
        let starts = bridge.starts.count
        let permissions = bridge.permissionRequests
        f.publish("active", raw: "active", port: 5002, token: "replacement", callID: "same-call")
        AudioBridge.shared.onDefaultDeviceChange?()
        try? await Task.sleep(for: .milliseconds(450))
        expect(bridge.starts.count == starts && bridge.permissionRequests == permissions,
               "same callID with changed token/port bypassed hangup fence")
        expect(!f.store.running && !f.store.readerMicrophoneActive && bridge.connected == nil,
               "same callID route change reopened audio after hangup")
        print("PASS: same callID retains hangup fence despite changed token and port")
    }
    @MainActor static func pendingHangupFencesLateAttachment() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared; bridge.holdStarts = true
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("held attachment before hangup", { bridge.waitingStarts.count == 1 })
        let old = bridge.waitingStarts[0]
        f.store.prepareForHangup(endpoint: "reader")
        f.publish("active", raw: "active")
        bridge.completeStart(old)
        await settle()
        f.publish("active", raw: "active")
        await settle()
        expect(bridge.starts.count == 1 && bridge.enableCalls == 0 && bridge.permissionRequests == 0,
               "late attachment or ACK opened a new microphone after hangup intent")
        expect(!f.store.running && !f.store.activationInFlight && !f.store.readerMicrophoneActive && bridge.connected == nil,
               "late attachment revived pending hangup")
        print("PASS: pending hangup survives late attachment and subsequent active polls")
    }
    @MainActor static func pendingHangupFencesLateMicrophoneGrant() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("playback before permission wait", { f.store.running && !f.store.activationInFlight })
        bridge.holdMicrophone = true
        f.publish("active", raw: "active")
        await waitFor("held permission before hangup", { bridge.waitingMicrophones.count == 1 })
        let old = bridge.waitingMicrophones[0]
        f.store.prepareForHangup(endpoint: "reader")
        f.publish("active", raw: "active")
        bridge.completeMicrophone(old)
        await settle()
        f.publish("active", raw: "active")
        await settle()
        expect(bridge.starts.count == 1 && bridge.enableCalls == 1 && bridge.permissionRequests == 1,
               "pending hangup requested capture again after a late grant")
        expect(!f.store.running && !f.store.readerMicrophoneActive && !bridge.microphone && bridge.connected == nil,
               "late grant revived pending hangup")
        print("PASS: pending hangup survives late permission completion and subsequent active polls")
    }
    @MainActor static func pendingHangupReleasesForNewCall() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared; bridge.holdStarts = true
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("old pending attachment", { bridge.waitingStarts.count == 1 })
        let old = bridge.waitingStarts[0]
        f.store.prepareForHangup(endpoint: "reader")
        f.publish("dialing", raw: "connecting", early: true, port: 5002, token: "new-call-token", callID: "new-call")
        await waitFor("new call admitted", { bridge.waitingStarts.count == 2 })
        let fresh = bridge.waitingStarts.first { $0 != old }!
        bridge.completeStart(fresh)
        await waitFor("new call playback attached", { f.store.running && !f.store.activationInFlight })
        bridge.completeStart(old)
        await settle()
        f.publish("active", raw: "active", port: 5002, token: "new-call-token", callID: "new-call")
        await waitFor("new call promoted", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 2 && bridge.connected?.id == fresh && bridge.enableCalls == 1,
               "old hangup fence blocked or retired the new call")
        print("PASS: a new call identity releases hangup fence; retired completion cannot disturb it")
    }
    @MainActor static func pendingHangupReleasesAtTerminalOrReset() async {
        for terminal in ["ended", "failed", "reset"] {
            let f = Fixture()
            let bridge = ReaderVoWiFiAudioBridge.shared
            f.publish("dialing", raw: "connecting", early: true)
            await waitFor("playback before terminal boundary", { f.store.running && !f.store.activationInFlight })
            f.store.prepareForHangup(endpoint: "reader")
            if terminal == "reset" { f.store.reset() }
            else { f.publish("idle", raw: terminal, ready: false) }
            // Reusing identity here deliberately proves release at the explicit
            // terminal/reset boundary, independently of new-identity handling.
            f.publish("active", raw: "active")
            await waitFor("audio after terminal/reset boundary", { f.store.running && f.store.readerMicrophoneActive && !f.store.activationInFlight })
            expect(bridge.starts.count == 2 && bridge.starts.last?.capture == true,
                   "terminal/reset did not release hangup fence")
            f.finish()
        }
        print("PASS: explicit ended/failed status and reset release the hangup fence")
    }
    @MainActor static func pendingHangupUsesTokenWhenCallIDIsUnavailable() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("active", raw: "active", callID: nil)
        await waitFor("token-only call attached", { f.store.running && !f.store.activationInFlight })
        f.store.prepareForHangup(endpoint: "reader")
        f.publish("active", raw: "active", callID: nil)
        await settle()
        expect(bridge.starts.count == 1 && !f.store.running, "same token bypassed hangup without callID")
        f.publish("active", raw: "active", token: "new-token", callID: nil)
        await waitFor("replacement token attached", { f.store.running && !f.store.activationInFlight })
        expect(bridge.starts.count == 2 && bridge.connected?.token == "new-token", "new token did not release identity fallback")
        print("PASS: absent callID uses the retained PCM token to distinguish replacement calls")
    }
    @MainActor static func moduleHangupAdmissionIsPreserved() async {
        let f = Fixture(); defer { f.finish() }
        f.endpoint = "module"
        f.publish("active")
        await waitFor("module before hangup", { f.store.running && !f.store.activationInFlight })
        f.store.prepareForHangup(endpoint: "module")
        f.publish("active")
        await settle()
        expect(!f.store.running && AudioBridge.shared.starts == 1, "same active module status changed preexisting admission")
        f.publish("idle")
        f.publish("active")
        await waitFor("next module call", { f.store.running && !f.store.activationInFlight })
        expect(AudioBridge.shared.starts == 2 && ReaderVoWiFiAudioBridge.shared.starts.isEmpty,
               "reader hangup fence affected module route admission")
        print("PASS: module hangup retains existing idle-to-active admission semantics")
    }
    @MainActor static func replacementIncomingRetiresOldCaptureWithoutIdle() async {
        for endpoint in ["reader", "module-vowifi"] {
            for changed in ["callID", "token", "port"] {
                let f = Fixture()
                f.endpoint = endpoint
                let bridge = ReaderVoWiFiAudioBridge.shared
                f.publish("active", raw: "active")
                await waitFor("old active capture", { f.store.running && f.store.readerMicrophoneActive && !f.store.activationInFlight })
                let nextID = changed == "callID" ? "replacement-call" : "fixture-call"
                let nextToken = changed == "token" ? "replacement-token" : "fixture-1"
                let nextPort = changed == "port" ? 5002 : 5001
                // Exactly the production sequence: a poll sees the new incoming
                // call before it has media, with no intervening idle snapshot.
                f.publish("incoming", raw: "incoming", ready: false,
                          port: nextPort, token: nextToken, callID: nextID)
                expect(!f.store.running && !f.store.readerMicrophoneActive && bridge.connected == nil && !bridge.microphone,
                       "replacement incoming retained the old call's live capture")
                f.publish("dialing", raw: "connecting", early: true,
                          port: nextPort, token: nextToken, callID: nextID)
                await waitFor("replacement RX attachment", { f.store.running && !f.store.activationInFlight })
                expect(bridge.starts.count == 2 && bridge.starts.last?.capture == false && bridge.starts.last?.ringback == false,
                       "replacement reused old audio or captured before ACK")
                expect(!f.store.readerMicrophoneActive && !bridge.microphone && bridge.permissionRequests == 1,
                       "replacement playback inherited old microphone state")
                let replacement = bridge.connected!.id
                f.publish("active", raw: "active", port: nextPort, token: nextToken, callID: nextID)
                await waitFor("replacement active capture", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
                expect(bridge.starts.count == 2 && bridge.connected?.id == replacement && bridge.enableCalls == 1,
                       "replacement ACK did not promote its own connection exactly once")
                f.finish()
            }
        }
        print("PASS: callID/token/port replacement retires active capture before new incoming media, without idle")
    }
    @MainActor static func oldMicrophoneGrantCannotAlterReplacementPlayback() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = ReaderVoWiFiAudioBridge.shared
        f.publish("dialing", raw: "connecting", early: true)
        await waitFor("old playback before delayed capture", { f.store.running && !f.store.activationInFlight })
        bridge.holdMicrophone = true
        f.publish("active", raw: "active")
        await waitFor("old microphone permission pending", { bridge.waitingMicrophones.count == 1 })
        let oldGrant = bridge.waitingMicrophones[0]
        f.publish("incoming", raw: "incoming", ready: false,
                  port: 5002, token: "replacement-token", callID: "replacement-call")
        expect(!f.store.running && !f.store.readerMicrophoneActive && bridge.connected == nil,
               "replacement did not cancel old microphone activation")
        f.publish("dialing", raw: "connecting", early: true,
                  port: 5002, token: "replacement-token", callID: "replacement-call")
        await waitFor("replacement playback while old grant pending", { f.store.running && !f.store.activationInFlight })
        let replacement = bridge.connected!.id
        bridge.completeMicrophone(oldGrant)
        await settle()
        expect(f.store.running && !f.store.readerMicrophoneActive && !bridge.microphone && !f.store.activationInFlight,
               "retired microphone grant modified replacement playback")
        expect(bridge.connected?.id == replacement && bridge.enableCalls == 1 && f.store.error == nil,
               "retired grant stopped or reported failure on the new route")
        bridge.holdMicrophone = false
        f.publish("active", raw: "active", port: 5002, token: "replacement-token", callID: "replacement-call")
        await waitFor("replacement microphone after its ACK", { f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(bridge.starts.count == 2 && bridge.connected?.id == replacement && bridge.enableCalls == 2,
               "replacement capture did not use its own permission operation")
        print("PASS: old delayed microphone grant cannot alter a replacement incoming RX-only connection")
    }
    @MainActor static func moduleHangupFencesDelayedPermissionAndActivePolls() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = AudioBridge.shared
        f.endpoint = "module"
        bridge.holdPermission = true
        f.publish("active")
        await waitFor("module permission held", { bridge.waitingPermissions.count == 1 })
        let oldGrant = bridge.waitingPermissions[0]
        f.store.prepareForHangup(endpoint: "module")
        for _ in 0..<3 {
            f.publish("active")
            f.store.startModuleAudio() // Also guard the explicit retry entry.
        }
        bridge.completePermission(oldGrant)
        await settle()
        expect(APITrace.paths.isEmpty && bridge.starts == 0 && bridge.permissionRequests == 1 && !f.store.running && !f.store.activationInFlight,
               "late module permission or active poll issued backendStart after hangup")
        // Nonterminal uncertainty must not turn a subsequent active poll into
        // a new call boundary, even though previous.isActive becomes false.
        for stale in ["unknown", "incoming", "dialing", "alerting"] {
            f.publish(stale)
            f.store.startModuleAudio()
            f.publish("active")
        }
        await settle()
        expect(APITrace.paths.isEmpty && bridge.permissionRequests == 1,
               "module active status after unknown bypassed the hangup fence")
        bridge.holdPermission = false
        f.publish("idle")
        f.publish("active")
        await waitFor("new module call after terminal", { f.store.running && !f.store.activationInFlight })
        expect(APITrace.paths == ["api/call/audio/start"] && bridge.starts == 1,
               "module terminal boundary did not permit a new call")
        f.store.prepareForHangup(endpoint: "module")
        f.store.reset()
        f.store.startModuleAudio()
        await waitFor("module audio after reset", { f.store.running && !f.store.activationInFlight })
        expect(APITrace.paths == ["api/call/audio/start", "api/call/audio/stop", "api/call/audio/start"] && bridge.starts == 2,
               "reset retained a stale module hangup fence")
        print("PASS: module pending hangup blocks late permission/backendStart and repeated active polls; idle/reset recover")
    }
    @MainActor static func modulePermissionChecksCurrentStateBeforeBackendStart() async {
        for replacement in ["idle", "unknown", "reader"] {
            let f = Fixture()
            let bridge = AudioBridge.shared
            f.endpoint = "module"
            bridge.holdPermission = true
            f.publish("active")
            await waitFor("controlled module permission", { bridge.waitingPermissions.count == 1 })
            let grant = bridge.waitingPermissions[0]
            if replacement == "reader" {
                f.endpoint = "reader"
                f.publish("incoming", raw: "incoming", ready: false)
            } else { f.publish(replacement) }
            bridge.completePermission(grant)
            await settle()
            expect(!APITrace.paths.contains("api/call/audio/start") && bridge.starts == 0 && !f.store.running && !f.store.activationInFlight,
                   "module permission triggered a backend start for an inactive/replaced route")
            f.finish()
        }
        print("PASS: module permission completion revalidates active state and endpoint before backendStart")
    }
    @MainActor static func retiredModulePermissionCannotAffectNewCall() async {
        let f = Fixture(); defer { f.finish() }
        let bridge = AudioBridge.shared
        f.endpoint = "module"
        bridge.holdPermission = true
        f.publish("active")
        await waitFor("old module permission", { bridge.waitingPermissions.count == 1 })
        let old = bridge.waitingPermissions[0]
        f.store.prepareForHangup(endpoint: "module")
        f.publish("idle")
        f.publish("active")
        await waitFor("new module permission", { bridge.waitingPermissions.count == 2 })
        let fresh = bridge.waitingPermissions.first { $0 != old }!
        bridge.completePermission(fresh)
        await waitFor("new module route started", { f.store.running && !f.store.activationInFlight })
        bridge.completePermission(old)
        await settle()
        expect(f.store.running && !f.store.activationInFlight && f.store.error == nil,
               "retired module permission altered the new call")
        expect(APITrace.paths == ["api/call/audio/start"] && bridge.starts == 1,
               "retired module permission restarted the backend or local audio")
        print("PASS: retired module permission cannot issue backendStart or disturb a newer module call")
    }
    @MainActor static func readerEndpointFallbackStopsCaptureBeforeUnknownModuleStatus() async {
        let f = Fixture(); defer { f.finish() }
        let reader = ReaderVoWiFiAudioBridge.shared
        f.publish("active", raw: "active")
        await waitFor("active reader before polling fallback", { f.store.running && f.store.readerMicrophoneActive && !f.store.activationInFlight })
        expect(reader.microphone && reader.connected != nil, "reader capture was never established")
        // CallRuntimeStore drops readerState and selects module after repeated
        // unavailable-reader polls. A failed module status request publishes
        // unknown, without an intervening idle state.
        f.endpoint = "module"
        f.publish("unknown")
        await settle()
        expect(reader.connected == nil && !reader.microphone && !f.store.readerMicrophoneActive,
               "fallback to module unknown retained reader capture")
        expect(!f.store.running && !f.store.activationInFlight && APITrace.paths.isEmpty && AudioBridge.shared.starts == 0,
               "unknown fallback started a module route or retained reader audio")
        f.publish("active")
        await waitFor("module active after fallback", { f.store.running && !f.store.activationInFlight })
        expect(APITrace.paths == ["api/call/audio/start"] && AudioBridge.shared.starts == 1 && reader.connected == nil,
               "module activation after reader fallback did not start its own route")
        print("PASS: reader-to-module unknown fallback stops old capture with no module side effect; later module active starts normally")
    }
    @MainActor static func readerEndpointFallbackDirectlyToActiveModuleStartsOnce() async {
        let f = Fixture(); defer { f.finish() }
        let reader = ReaderVoWiFiAudioBridge.shared
        let module = AudioBridge.shared
        f.publish("active", raw: "active")
        await waitFor("reader active before direct module fallback", { f.store.running && f.store.readerMicrophoneActive && !f.store.activationInFlight })
        f.endpoint = "module"
        f.publish("active") // previous.isActive is true, but belonged to reader.
        expect(reader.connected == nil && !reader.microphone && !f.store.readerMicrophoneActive,
               "direct active fallback retained the old reader route")
        await waitFor("module route after direct active fallback", { f.store.running && !f.store.activationInFlight })
        expect(APITrace.paths == ["api/call/audio/start"] && module.starts == 1 && module.permissionRequests == 1,
               "direct active fallback did not start exactly one module route")
        for _ in 0..<3 { f.publish("active") }
        await settle()
        expect(APITrace.paths == ["api/call/audio/start"] && module.starts == 1 && module.permissionRequests == 1,
               "duplicate active module poll retried permission or route activation")
        expect(reader.starts.count == 1 && reader.connected == nil,
               "module fallback reopened reader audio")
        print("PASS: reader active -> module active retires reader and starts module once; duplicate active polls are inert")
    }
}
