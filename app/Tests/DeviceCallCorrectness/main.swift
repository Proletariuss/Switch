import Foundation

private final class FaultingCallCheckpointDefaults: CallCheckpointDefaults {
    var values: [String: Any] = [:]
    var synchronizeResult = true

    func object(forKey defaultName: String) -> Any? { values[defaultName] }
    func set(_ value: Any?, forKey defaultName: String) { values[defaultName] = value }
    func removeObject(forKey defaultName: String) { values.removeValue(forKey: defaultName) }
    func synchronize() -> Bool { synchronizeResult }
}

@MainActor private func wait(file: StaticString = #fileID, line: UInt = #line, _ condition: () -> Bool) async {
    for _ in 0..<500 where !condition() { try? await Task.sleep(nanoseconds: 10_000_000) }
    precondition(condition(), "fixture condition did not become true", file: file, line: line)
}

// Observe actual request admission, never elapsed scheduler turns.
@MainActor private func pending(
    _ path: String, after baseline: Int, file: StaticString = #fileID, line: UInt = #line
) async -> Int {
    let api = ControlledAPI.shared
    func matches() -> [Int] {
        api.requests.indices.filter { $0 >= baseline && api.requests[$0].path == path && api.isPending($0) }
    }
    await wait(file: file, line: line) { !matches().isEmpty }
    let indices = matches()
    precondition(indices.count == 1, "multiple new pending requests for \(path)", file: file, line: line)
    return indices[0]
}

@MainActor private func issuing(
    _ path: String, file: StaticString = #fileID, line: UInt = #line, _ action: () -> Void
) async -> Int {
    let baseline = ControlledAPI.shared.requests.count
    action()
    return await pending(path, after: baseline, file: file, line: line)
}

// Tests inspect existing lifecycle state without replacing the production Store.
// Missing/renamed fields fail closed instead of silently skipping the barrier.
@MainActor private func field<T>(_ object: Any, _ name: String, as type: T.Type) -> T {
    guard let child = Mirror(reflecting: object).children.first(where: { $0.label == name }),
          let value = child.value as? T else {
        preconditionFailure("missing production lifecycle field \(name)")
    }
    return value
}
@MainActor private func retainedTask(_ object: Any, _ name: String) -> Task<Void, Never> {
    let value: Task<Void, Never>? = field(object, name, as: Optional<Task<Void, Never>>.self)
    guard let value else { preconditionFailure("production task \(name) was not started") }
    return value
}
@MainActor private func retired(_ task: Task<Void, Never>) async {
    var done = false
    let join = Task { await task.value; done = true }
    await wait { done }
    await join.value
}
@MainActor private func actionRetired(_ runtime: CallRuntimeStore) async {
    await wait { !field(runtime, "pendingRequestInFlight", as: Bool.self) }
}
@MainActor private func pollRetired(_ runtime: CallRuntimeStore) async {
    await wait { !field(runtime, "statusRefreshInFlight", as: Bool.self) }
}
@MainActor private func drain(_ runtime: CallRuntimeStore) async {
    await wait {
        ControlledAPI.shared.finishPending()
        return !field(runtime, "pendingRequestInFlight", as: Bool.self)
            && !field(runtime, "statusRefreshInFlight", as: Bool.self)
    }
}

@main private enum CorrectnessTests {
    @MainActor static func main() async throws {
        let isolationPrefix = "switch.call-correctness.\(UUID().uuidString)"
        let isolatedSuites = ["runtime", "dial-boundary", "history", "owner", "polled-module-owner"]
        func isolatedDefaults(_ suffix: String) -> UserDefaults {
            let name = "\(isolationPrefix).\(suffix)"
            let defaults = UserDefaults(suiteName: name)!
            defaults.removePersistentDomain(forName: name)
            return defaults
        }
        defer {
            for suffix in isolatedSuites {
                UserDefaults.standard.removePersistentDomain(
                    forName: "\(isolationPrefix).\(suffix)")
            }
        }
        let legacyCall = try JSONDecoder().decode(CallStatus.self,
            from: Data(#"{"state":"idle"}"#.utf8))
        let degradedCall = try JSONDecoder().decode(CallStatus.self,
            from: Data(#"{"state":"active","history_error":"通话记录暂未确认"}"#.utf8))
        precondition(legacyCall.historyError == nil && degradedCall.isActive
                     && degradedCall.historyError == "通话记录暂未确认",
                     "history failure must be optional and independent of physical call state")
        let legacyReaderCall = try JSONDecoder().decode(ReaderVoWiFiCallState.self,
            from: Data(#"{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}"#.utf8))
        let measuredReaderCall = try JSONDecoder().decode(ReaderVoWiFiCallState.self,
            from: Data(#"{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true,"rtp_packets_sent":10,"rtp_packets_received":8,"rtp_decode_errors":1,"audio_frames_to_host":8}"#.utf8))
        precondition(legacyReaderCall.rtpPacketsSent == nil
                     && measuredReaderCall.rtpPacketsReceived == 8
                     && measuredReaderCall.rtpDecodeErrors == 1
                     && measuredReaderCall.audioFramesToHost == 8,
                     "RTP observations must be optional for older reader status payloads")
        let api = ControlledAPI.shared
        let runtime = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("runtime"))
        runtime.moduleCallModeReady = { true }
        let firstDial = await issuing("api/call/dial") {
            runtime.dial("12345")
            let revision = field(runtime, "actionRevision", as: UInt64.self)
            runtime.dial("12345")
            precondition(field(runtime, "actionRevision", as: UInt64.self) == revision,
                         "duplicate dial started another action")
        }
        precondition(api.requests.count == 1 && runtime.dialInFlight, "duplicate dial submitted")
        let firstPollBaseline = api.requests.count
        api.complete(firstDial, json: #"{"accepted":true}"#)
        await actionRetired(runtime)
        let firstPoll = await pending("api/call/status", after: firstPollBaseline)
        precondition(runtime.status.state == "dialing" && !runtime.dialInFlight,
                     "accepted dial must lock the UI before the status poll completes")
        let acceptedRevision = field(runtime, "actionRevision", as: UInt64.self)
        runtime.dial("67890")
        precondition(field(runtime, "actionRevision", as: UInt64.self) == acceptedRevision)
        precondition(api.requests.count == 2, "accepted call permitted another dial")
        api.complete(firstPoll, json: #"{"state":"dialing","number":"12345"}"#)
        await pollRetired(runtime)
        runtime.reset()
        let late = await issuing("api/call/dial") { runtime.dial("12345") }
        runtime.reset()
        api.complete(late, json: #"{"accepted":true}"#)
        await actionRetired(runtime)
        precondition(runtime.status.isIdle && !runtime.commandInFlight, "late dial revived reset state")

        runtime.endpoint = "reader"
        runtime.status = CallStatus(state: "incoming", number: "fixture", incoming: true, active: false)
        let store = DashboardStore(runtime)
        let card = CardFixture()
        func incoming(_ ready: Bool) throws -> ReaderVoWiFiCallState {
            try JSONDecoder().decode(ReaderVoWiFiCallState.self, from: Data("""
                {"state":"incoming","signaling_ready":\(ready),"media_ready":false,"early_media":false}
                """.utf8))
        }
        runtime.readerState = try incoming(false)
        card.synchronize(store: store)
        precondition(!card.answerAvailable)
        runtime.readerState = try incoming(true)
        card.synchronize(store: store)
        precondition(card.answerAvailable, "same incoming call did not become actionable")
        let rejectedBaseline = api.requests.count
        runtime.reject()
        card.synchronize(store: store)
        precondition(card.rejectInFlight && !card.answerAvailable)
        let rejected = await pending("api/call/hangup", after: rejectedBaseline)
        api.fail(rejected)
        await actionRetired(runtime)
        card.synchronize(store: store)
        precondition(!card.rejectInFlight && card.answerAvailable && card.failureMessage != nil,
                     "failed reject must remain actionable and visible")
        await drain(runtime)
        runtime.reset()

        let dialBoundary = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("dial-boundary"))
        dialBoundary.moduleCallModeReady = { true }
        let rawDial = await issuing("api/call/dial") {
            dialBoundary.dial(" 00 44 (20) 7946-0000 ")
        }
        let rawDialBody = try JSONSerialization.jsonObject(
            with: api.requests[rawDial].body ?? Data()) as! [String: Any]
        precondition(api.requests[rawDial].path == "api/call/dial"
                     && rawDialBody["number"] as? String == "00 44 (20) 7946-0000",
                     "Swift must transport the dial text unchanged; Rust owns canonicalization")
        api.complete(rawDial, json: #"{"accepted":true}"#)
        await actionRetired(dialBoundary)
        await drain(dialBoundary)
        dialBoundary.reset()

        runtime.endpoint = "module"
        runtime.status = CallStatus(state: "incoming", number: "fixture", incoming: true, active: false)
        let negativeReject = await issuing("api/call/hangup") { runtime.reject() }
        api.complete(negativeReject, json: #"{"accepted":false}"#)
        await actionRetired(runtime)
        card.synchronize(store: store)
        precondition(!runtime.rejectInFlight && runtime.error?.contains("没有确认") == true
                     && card.failureMessage != nil,
                     "negative module acknowledgement must remain a visible reject failure")
        await drain(runtime)
        runtime.reset()

        let diagnostic = DiagnosticFixture()
        let old = await issuing("api/network") {
            diagnostic.loadDiagnostic()
            let revision = diagnostic.collectionRevision
            diagnostic.loadDiagnostic()
            precondition(diagnostic.collectionRevision == revision, "duplicate diagnostic started another task")
        }
        let oldDiagnosticTask = retainedTask(diagnostic, "collectionTask")
        diagnostic.cancelCollection()
        let current = await issuing("api/network") { diagnostic.loadDiagnostic() }
        let currentDiagnosticTask = retainedTask(diagnostic, "collectionTask")
        precondition(current == old + 1, "diagnostic must be single-flight")
        api.complete(current, json: #"{"usbnet_mode":"1","partial":true,"captured_at_ms":1000,"duration_ms":20,"completed_commands":1}"#)
        await retired(currentDiagnosticTask)
        api.complete(old, json: #"{"usbnet_mode":"0"}"#)
        await retired(oldDiagnosticTask)
        precondition(diagnostic.diagnostic?.usbnetMode == "1" && diagnostic.diagnostic?.partial == true,
                     "canceled diagnostic overwrote newer partial result")
        let failedDiagnostic = await issuing("api/network") { diagnostic.loadDiagnostic() }
        let failedDiagnosticTask = retainedTask(diagnostic, "collectionTask")
        api.fail(failedDiagnostic)
        await retired(failedDiagnosticTask)
        precondition(diagnostic.diagnostic?.usbnetMode == "1" && diagnostic.diagnosticError != nil,
                     "diagnostic failure must retain the last completed result")
        diagnostic.backend.state = .stopped
        let offlineRequestCount = api.requests.count
        diagnostic.loadDiagnostic()
        precondition(diagnostic.collectionTask == nil, "offline collection scheduled a task")
        precondition(api.requests.count == offlineRequestCount && diagnostic.diagnostic?.usbnetMode == "1",
                     "offline collection must retain the labeled historical result without I/O")

        let console = ATConsoleFixture()
        let canceledAT = await issuing("api/at") {
            console.command = "AT+CSQ"
            console.sendAT()
            let revision = console.atRevision
            console.command = "AT+CSQ"
            console.sendAT()
            precondition(console.atRevision == revision, "duplicate AT started another task")
        }
        let canceledATTask = retainedTask(console, "atTask")
        console.cancelATWait()
        api.complete(canceledAT, json: #"{"response":"late result"}"#)
        await retired(canceledATTask)
        precondition(console.history.isEmpty && !console.atBusy,
                     "retired AT response must not write to a closed console")

        let networkBaseline = api.requests.count
        let network = ModuleNetworkStore(backend: BackendProcess())
        // Combine's initial receive(on: RunLoop.main) is also asynchronous.
        let initialNetwork = await pending("api/network", after: networkBaseline)
        let initialFlight = await pending("api/module/flight-mode", after: networkBaseline)
        let initialNetworkTask = retainedTask(network, "loadTask")
        let initialFlightTask = retainedTask(network, "flightModeTask")
        api.fail(initialNetwork)
        api.fail(initialFlight)
        await retired(initialNetworkTask)
        await retired(initialFlightTask)
        let failedSwitch = await issuing("api/network/usbnet") { network.switchMode() }
        let failedSwitchTask = retainedTask(network, "actionTask")
        let failedRefreshBaseline = api.requests.count
        api.fail(failedSwitch)
        await retired(failedSwitchTask)
        precondition(network.actionError != nil && !network.busy && !network.usbnetEnabled,
                     "failed network action must show failure and retain observed configuration")
        let failedRefresh = await pending("api/network", after: failedRefreshBaseline)
        let failedRefreshTask = retainedTask(network, "loadTask")
        api.fail(failedRefresh)
        await retired(failedRefreshTask)
        let successfulSwitch = await issuing("api/network/usbnet") { network.switchMode() }
        let successfulSwitchTask = retainedTask(network, "actionTask")
        let successRefreshBaseline = api.requests.count
        api.complete(successfulSwitch, json: #"{"mode":1,"needs_reboot":true}"#)
        await retired(successfulSwitchTask)
        precondition(network.needsReboot && network.actionMessage?.contains("重启") == true,
                     "needs_reboot acknowledgement must not claim an applied network change")
        let successRefresh = await pending("api/network", after: successRefreshBaseline)
        let successRefreshTask = retainedTask(network, "loadTask")
        network.reset()
        api.fail(successRefresh)
        await retired(successRefreshTask)

        let summary = SafeDiagnosticSummary(version: "fixture", build: "1", backendState: "running",
            lastExit: nil, moduleSnapshotStale: false, readerConnected: true,
            moduleSMSReady: false, readerSMSReady: true, collectedAt: nil,
            network: .init(partial: true, durationMS: 20, completedCommands: 1,
                failedCommands: ["cgpaddr", "SMS_BODY"], usbNetworkPresent: true, interfaceCount: 1))
        let exported = String(decoding: try summary.data(), as: UTF8.self)
        precondition(!exported.contains("SECRET_TOKEN") && !exported.contains("SMS_BODY"),
                     "export leaked untrusted text outside its allowlist")
        let historyRuntime = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("history"))
        historyRuntime.endpoint = "reader"
        historyRuntime.readerAvailable = { true }
        let historyStatus = await issuing("api/vowifi/calls/status") { historyRuntime.poll() }
        api.complete(historyStatus, json: #"{"generation":1,"source":"reader","history_error":"通话记录暂未确认","call":{"state":"ended","media_ready":false,"early_media":false,"signaling_ready":true}}"#)
        await pollRetired(historyRuntime)
        precondition(historyRuntime.status.isIdle
                     && historyRuntime.status.historyError == "通话记录暂未确认",
                     "ended IMS call must keep the archive failure visible")
        historyRuntime.reset()
        precondition(historyRuntime.status.historyError == nil)

        let polledModule = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("polled-module-owner"))
        let initialModuleStatus = await issuing("api/call/status") { polledModule.poll() }
        api.complete(initialModuleStatus, json: #"{"state":"active","number":"module-call"}"#)
        await pollRetired(polledModule)
        precondition(polledModule.status.isActive)
        polledModule.readerAvailable = { true }
        let competingReader = await issuing("api/vowifi/calls/status") { polledModule.poll() }
        let preservedModuleBaseline = api.requests.count
        api.complete(competingReader, json: #"{"generation":99,"source":"reader","call":{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        let preservedModule = await pending("api/call/status", after: preservedModuleBaseline)
        api.complete(preservedModule, json: #"{"state":"active","number":"module-call"}"#)
        await pollRetired(polledModule)
        polledModule.selectEndpoint("reader")
        precondition(polledModule.endpoint == "module" && polledModule.status.number == "module-call",
                     "VoWiFi poll/UI selection stole the module owner first observed by polling")
        let competingAgain = await issuing("api/vowifi/calls/status") { polledModule.poll() }
        let terminalModuleBaseline = api.requests.count
        api.complete(competingAgain, json: #"{"generation":99,"source":"reader","call":{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        let terminalModule = await pending("api/call/status", after: terminalModuleBaseline)
        api.complete(terminalModule, json: #"{"state":"idle"}"#)
        await pollRetired(polledModule)
        precondition(polledModule.status.isIdle)
        let successorStatus = await issuing("api/vowifi/calls/status") { polledModule.poll() }
        api.complete(successorStatus, json: #"{"generation":100,"source":"module","call":{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        await pollRetired(polledModule)
        precondition(polledModule.endpoint == "module-vowifi" && polledModule.status.isActive)
        polledModule.reset()
        print("PASS: first module poll binds endpoint owner; later VoWiFi cannot steal it before module terminal idle")

        let ownerRuntime = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("owner"))
        ownerRuntime.selectEndpoint("reader")
        ownerRuntime.readerAvailable = { true }
        let ownerStatus = await issuing("api/vowifi/calls/status") { ownerRuntime.poll() }
        api.complete(ownerStatus, json: #"{"generation":77,"source":"reader","call":{"state":"active","peer":"+85256600731","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        await pollRetired(ownerRuntime)
        precondition(ownerRuntime.status.isActive)
        for _ in 0..<3 {
            let failedPoll = await issuing("api/vowifi/calls/status") { ownerRuntime.poll() }
            api.fail(failedPoll)
            await pollRetired(ownerRuntime)
        }
        ownerRuntime.selectEndpoint("module")
        precondition(ownerRuntime.endpoint == "reader" && ownerRuntime.status.isActive,
                     "failed observations or UI selection must not move an active Rust call to another owner")
        let hangup = await issuing("api/call/hangup") { ownerRuntime.hangup() }
        let hangupBody = try JSONSerialization.jsonObject(
            with: api.requests[hangup].body ?? Data()) as! [String: Any]
        precondition(api.requests[hangup].path == "api/call/hangup"
                     && hangupBody["observed_owner"] as? String == "reader"
                     && hangupBody["generation"] as? Int == 77,
                     "hangup lost the Rust reader owner receipt after failed observations")
        api.complete(hangup, json: #"{"generation":77,"source":"reader","call":{"state":"ended","media_ready":false,"early_media":false,"signaling_ready":true}}"#)
        await actionRetired(ownerRuntime)
        await drain(ownerRuntime)

        let suiteName = "switch.call-receipt-fixture.\(UUID().uuidString)"
        let durableDefaults = UserDefaults(suiteName: suiteName)!
        durableDefaults.removePersistentDomain(forName: suiteName)
        let beforeDurable = api.requests.count
        let firstGUI = CallRuntimeStore(backend: BackendProcess(), defaults: durableDefaults)
        firstGUI.moduleCallModeReady = { true }
        let firstSubmit = await issuing("api/call/dial") { firstGUI.dial("15550001111") }
        precondition(firstSubmit >= beforeDurable && api.requests[firstSubmit].path == "api/call/dial")
        let firstBody = api.requests[firstSubmit].body ?? Data()
        let firstJSON = try JSONSerialization.jsonObject(with: firstBody) as! [String: Any]
        let durableID = firstJSON["client_request_id"] as? String
        precondition(durableID?.isEmpty == false, "dial did not carry a client request identity")
        api.failUnknown(firstSubmit)
        await actionRetired(firstGUI)
        precondition(firstGUI.dialInFlight && firstGUI.commandInFlight
                     && firstGUI.error?.contains("原请求编号") == true,
                     "unknown dial response released the durable fence")
        let fencedCount = api.requests.count
        firstGUI.dial("15550002222")
        firstGUI.reset()
        precondition(!field(firstGUI, "pendingRequestInFlight", as: Bool.self))
        precondition(api.requests.count == fencedCount && firstGUI.dialInFlight,
                     "reset or a new dial replaced an unresolved request identity")

        let secondGUI = CallRuntimeStore(backend: BackendProcess(), defaults: durableDefaults)
        secondGUI.moduleCallModeReady = { true }
        precondition(secondGUI.dialInFlight && secondGUI.commandInFlight,
                     "GUI restart did not restore the unknown fence")
        let recoveryPollBaseline = api.requests.count
        let replaySubmit = await issuing("api/call/dial") { secondGUI.poll() }
        let recoveryStatus = await pending("api/call/status", after: recoveryPollBaseline)
        precondition(api.requests[replaySubmit].body == firstBody,
                     "GUI restart re-encoded or replaced the durable call payload")
        let replayJSON = try JSONSerialization.jsonObject(
            with: api.requests[replaySubmit].body ?? Data()) as! [String: Any]
        precondition(replayJSON["client_request_id"] as? String == durableID,
                     "GUI restart generated a new request identity")
        api.complete(replaySubmit, json: #"{"phase":"unknown","outcome":"unknown","selected_owner":"module","generation":0,"dispatch_count":0}"#)
        await actionRetired(secondGUI)
        precondition(secondGUI.commandInFlight && secondGUI.dialInFlight,
                     "unbound admitted module receipt released the Swift fence")
        secondGUI.acceptModuleStatus(CallStatus(
            state: "incoming", number: "15550009999", incoming: true, active: false))
        secondGUI.selectEndpoint("reader")
        let beforeBlockedAction = api.requests.count
        secondGUI.answer()
        secondGUI.hangup()
        precondition(!field(secondGUI, "pendingRequestInFlight", as: Bool.self))
        precondition(api.requests.count == beforeBlockedAction && secondGUI.commandInFlight,
                     "next incoming call or owner selection bypassed the unknown mutation fence")
        api.fail(recoveryStatus)
        await pollRetired(secondGUI)
        secondGUI.reset()
        let terminalReplay = await issuing("api/call/dial") { secondGUI.poll() }
        precondition(api.requests[terminalReplay].body == firstBody,
                     "unknown admission recovery replaced the original exact payload")
        api.complete(terminalReplay, json: #"{"accepted":true}"#)
        await actionRetired(secondGUI)
        precondition(!secondGUI.commandInFlight && secondGUI.status.isIdle,
                     "old terminal replay either retained its fence or regressed the live projection")
        await drain(secondGUI)
        let thirdGUI = CallRuntimeStore(backend: BackendProcess(), defaults: durableDefaults)
        precondition(!thirdGUI.commandInFlight, "terminal call receipt survived as an unresolved fence")
        durableDefaults.removePersistentDomain(forName: suiteName)

        let failedPersistence = FaultingCallCheckpointDefaults()
        failedPersistence.synchronizeResult = false
        let persistenceRuntime = CallRuntimeStore(
            backend: BackendProcess(), defaults: failedPersistence)
        persistenceRuntime.moduleCallModeReady = { true }
        let beforePersistenceFailure = api.requests.count
        persistenceRuntime.dial("15550003333")
        precondition(!field(persistenceRuntime, "pendingRequestInFlight", as: Bool.self))
        precondition(persistenceRuntime.commandInFlight
                     && persistenceRuntime.error?.contains("持久保存") == true,
                     "failed checkpoint save did not retain a visible sticky fence")
        _ = await issuing("api/call/status") { persistenceRuntime.poll() }
        await drain(persistenceRuntime)
        persistenceRuntime.reset()
        failedPersistence.synchronizeResult = true
        _ = await issuing("api/call/status") {
            persistenceRuntime.poll()
            persistenceRuntime.dial("15550004444")
        }
        await drain(persistenceRuntime)
        precondition(
            api.requests[beforePersistenceFailure...].allSatisfy {
                !["api/call/dial", "api/call/answer", "api/call/hangup"].contains($0.path)
            },
            "save failure was bypassed by poll/reset/retry and uploaded a call mutation")
        await drain(persistenceRuntime)

        let nonDataCheckpoint = FaultingCallCheckpointDefaults()
        nonDataCheckpoint.values["switch.call.pendingMutation.v1"] = "wrong defaults type"
        let corruptRuntime = CallRuntimeStore(
            backend: BackendProcess(), defaults: nonDataCheckpoint)
        let beforeCorruptPoll = api.requests.count
        _ = await issuing("api/call/status") {
            corruptRuntime.poll()
            corruptRuntime.dial("15550005555")
        }
        await drain(corruptRuntime)
        precondition(corruptRuntime.commandInFlight
                     && corruptRuntime.error?.contains("损坏") == true
                     && api.requests[beforeCorruptPoll...].allSatisfy {
                         !["api/call/dial", "api/call/answer", "api/call/hangup"].contains($0.path)
                     },
                     "non-Data checkpoint was treated as missing or uploaded")
        await drain(corruptRuntime)

        // Real NetworkServicesStore with only transport completion controlled.
        // A cancelled waiter deliberately completes late to prove revision guards.
        for oldResult in ["success", "failure"] {
            let network = NetworkServicesStore(backend: BackendProcess())
            let initialLoad = await issuing("api/network/services") { network.load() }
            let initialLoadTask = retainedTask(network, "loadTask")
            api.complete(initialLoad, json: #"{"services":[{"name":"A"},{"name":"B"}]}"#)
            await retired(initialLoadTask)
            let oldSave = await issuing("api/network/services-order") { network.saveOrder() }
            let oldSaveTask = retainedTask(network, "saveTask")
            network.reset()
            let successorLoad = await issuing("api/network/services") { network.load() }
            let successorLoadTask = retainedTask(network, "loadTask")
            api.complete(successorLoad, json: #"{"services":[{"name":"C"},{"name":"D"}]}"#)
            await retired(successorLoadTask)
            let newSave = await issuing("api/network/services-order") { network.saveOrder() }
            let newSaveTask = retainedTask(network, "saveTask")
            let beforeOldCompletion = api.requests.count
            if oldResult == "success" {
                api.complete(oldSave, json: #"{"message":"obsolete"}"#)
            } else { api.fail(oldSave) }
            await retired(oldSaveTask)
            precondition(api.isPending(newSave),
                         "successor request must remain physically pending while old save retires")
            precondition(network.isSaving && network.message == nil && network.error == nil,
                         "old save completion cleared successor busy state or feedback")
            precondition(api.requests.count == beforeOldCompletion,
                         "old successful save started a refresh on the successor")
            let duplicateSaveRevision = field(network, "saveRevision", as: UInt64.self)
            network.saveOrder()
            precondition(field(network, "saveRevision", as: UInt64.self) == duplicateSaveRevision,
                         "duplicate save scheduled another task")
            precondition(api.requests.count == beforeOldCompletion && api.isPending(newSave),
                         "old defer allowed a duplicate successor save")
            let refreshBaseline = api.requests.count
            api.complete(newSave, json: #"{"message":"current"}"#)
            await retired(newSaveTask)
            precondition(!network.isSaving && network.message == "current")
            let refresh = await pending("api/network/services", after: refreshBaseline)
            let refreshTask = retainedTask(network, "loadTask")
            api.complete(refresh, json: #"{"services":[{"name":"C"},{"name":"D"}]}"#)
            await retired(refreshTask)

            let stoppedSave = await issuing("api/network/services-order") { network.saveOrder() }
            let stoppedSaveTask = retainedTask(network, "saveTask")
            network.reset()
            api.fail(stoppedSave)
            await retired(stoppedSaveTask)
            precondition(!network.isSaving && network.error == nil && network.services.isEmpty,
                         "late cancelled save republished feedback after reset")
        }
        print("PASS: network priority save success/failure/reset fence both publication and task retirement")

        print("PASS production call duplicate fencing/readiness/reject recovery, durable same-ID GUI restart, sticky persistence/non-Data fences, unbound module unknown receipt/next-call fence, history degradation, diagnostic cancellation/partial preservation, network error/manual reboot, allowlisted export")
    }
}
