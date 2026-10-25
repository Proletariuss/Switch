import Foundation

private final class FaultingCallCheckpointDefaults: CallCheckpointDefaults {
    var values: [String: Any] = [:]
    var synchronizeResult = true

    func object(forKey defaultName: String) -> Any? { values[defaultName] }
    func set(_ value: Any?, forKey defaultName: String) { values[defaultName] = value }
    func removeObject(forKey defaultName: String) { values.removeValue(forKey: defaultName) }
    func synchronize() -> Bool { synchronizeResult }
}

@MainActor private func settle() async { for _ in 0..<12 { await Task.yield() } }
@MainActor private func wait(_ condition: () -> Bool) async {
    for _ in 0..<500 where !condition() { try? await Task.sleep(nanoseconds: 10_000_000) }
    precondition(condition(), "fixture condition did not become true")
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
        runtime.dial("12345")
        runtime.dial("12345")
        await settle()
        precondition(api.requests.count == 1 && runtime.dialInFlight, "duplicate dial submitted")
        api.complete(0, json: #"{"accepted":true}"#)
        await settle()
        precondition(runtime.status.state == "dialing" && !runtime.dialInFlight,
                     "accepted dial must lock the UI before the status poll completes")
        runtime.dial("67890")
        precondition(api.requests.count == 2, "accepted call permitted another dial")
        api.complete(1, json: #"{"state":"dialing","number":"12345"}"#)
        await settle()
        runtime.reset()
        runtime.dial("12345")
        await settle()
        let late = api.requests.count - 1
        runtime.reset()
        api.complete(late, json: #"{"accepted":true}"#)
        await settle()
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
        runtime.reject()
        card.synchronize(store: store)
        precondition(card.rejectInFlight && !card.answerAvailable)
        await settle()
        api.fail(api.requests.count - 1)
        await settle()
        card.synchronize(store: store)
        precondition(!card.rejectInFlight && card.answerAvailable && card.failureMessage != nil,
                     "failed reject must remain actionable and visible")
        runtime.reset()
        api.finishPending()
        await settle()

        let dialBoundary = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("dial-boundary"))
        dialBoundary.moduleCallModeReady = { true }
        dialBoundary.dial(" 00 44 (20) 7946-0000 ")
        await settle()
        let rawDial = api.requests.count - 1
        let rawDialBody = try JSONSerialization.jsonObject(
            with: api.requests[rawDial].body ?? Data()) as! [String: Any]
        precondition(api.requests[rawDial].path == "api/call/dial"
                     && rawDialBody["number"] as? String == "00 44 (20) 7946-0000",
                     "Swift must transport the dial text unchanged; Rust owns canonicalization")
        api.complete(rawDial, json: #"{"accepted":true}"#)
        await settle()
        dialBoundary.reset()
        api.finishPending()
        await settle()

        runtime.endpoint = "module"
        runtime.status = CallStatus(state: "incoming", number: "fixture", incoming: true, active: false)
        runtime.reject()
        await settle()
        api.complete(api.requests.count - 1, json: #"{"accepted":false}"#)
        await settle()
        card.synchronize(store: store)
        precondition(!runtime.rejectInFlight && runtime.error?.contains("没有确认") == true
                     && card.failureMessage != nil,
                     "negative module acknowledgement must remain a visible reject failure")
        runtime.reset()
        api.finishPending()
        await settle()

        let diagnostic = DiagnosticFixture()
        diagnostic.loadDiagnostic()
        diagnostic.loadDiagnostic()
        await settle()
        let old = api.requests.count - 1
        diagnostic.cancelCollection()
        diagnostic.loadDiagnostic()
        await settle()
        let current = api.requests.count - 1
        precondition(current == old + 1, "diagnostic must be single-flight")
        api.complete(current, json: #"{"usbnet_mode":"1","partial":true,"captured_at_ms":1000,"duration_ms":20,"completed_commands":1}"#)
        await settle()
        api.complete(old, json: #"{"usbnet_mode":"0"}"#)
        await settle()
        precondition(diagnostic.diagnostic?.usbnetMode == "1" && diagnostic.diagnostic?.partial == true,
                     "canceled diagnostic overwrote newer partial result")
        diagnostic.loadDiagnostic()
        await settle()
        api.fail(api.requests.count - 1)
        await settle()
        precondition(diagnostic.diagnostic?.usbnetMode == "1" && diagnostic.diagnosticError != nil,
                     "diagnostic failure must retain the last completed result")
        diagnostic.backend.state = .stopped
        let offlineRequestCount = api.requests.count
        diagnostic.loadDiagnostic()
        await settle()
        precondition(api.requests.count == offlineRequestCount && diagnostic.diagnostic?.usbnetMode == "1",
                     "offline collection must retain the labeled historical result without I/O")

        let console = ATConsoleFixture()
        console.command = "AT+CSQ"
        console.sendAT()
        console.command = "AT+CSQ"
        console.sendAT()
        await settle()
        let canceledAT = api.requests.count - 1
        console.cancelATWait()
        api.complete(canceledAT, json: #"{"response":"late result"}"#)
        await settle()
        precondition(console.history.isEmpty && !console.atBusy,
                     "retired AT response must not write to a closed console")

        let network = ModuleNetworkStore(backend: BackendProcess())
        await settle()
        api.finishPending()
        await settle()
        network.switchMode()
        await settle()
        api.fail(api.requests.count - 1)
        await settle()
        precondition(network.actionError != nil && !network.busy && !network.usbnetEnabled,
                     "failed network action must show failure and retain observed configuration")
        api.finishPending()
        await settle()
        network.switchMode()
        await settle()
        api.complete(api.requests.count - 1, json: #"{"mode":1,"needs_reboot":true}"#)
        await wait { network.needsReboot }
        precondition(network.actionMessage?.contains("重启") == true,
                     "needs_reboot acknowledgement must not claim an applied network change")
        network.reset()
        api.finishPending()
        await settle()

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
        historyRuntime.poll()
        await settle()
        api.complete(api.requests.count - 1, json: #"{"generation":1,"source":"reader","history_error":"通话记录暂未确认","call":{"state":"ended","media_ready":false,"early_media":false,"signaling_ready":true}}"#)
        await wait { historyRuntime.status.historyError != nil }
        precondition(historyRuntime.status.isIdle
                     && historyRuntime.status.historyError == "通话记录暂未确认",
                     "ended IMS call must keep the archive failure visible")
        historyRuntime.reset()
        precondition(historyRuntime.status.historyError == nil)

        let polledModule = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("polled-module-owner"))
        polledModule.poll()
        await settle()
        precondition(api.requests.last?.path == "api/call/status")
        api.complete(api.requests.count - 1, json: #"{"state":"active","number":"module-call"}"#)
        await wait { polledModule.status.isActive }
        polledModule.readerAvailable = { true }
        polledModule.poll()
        await settle()
        api.complete(api.requests.count - 1, json: #"{"generation":99,"source":"reader","call":{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        await wait { api.requests.last?.path == "api/call/status" }
        api.complete(api.requests.count - 1, json: #"{"state":"active","number":"module-call"}"#)
        await settle()
        polledModule.selectEndpoint("reader")
        precondition(polledModule.endpoint == "module" && polledModule.status.number == "module-call",
                     "VoWiFi poll/UI selection stole the module owner first observed by polling")
        polledModule.poll()
        await settle()
        api.complete(api.requests.count - 1, json: #"{"generation":99,"source":"reader","call":{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        await wait { api.requests.last?.path == "api/call/status" }
        api.complete(api.requests.count - 1, json: #"{"state":"idle"}"#)
        await wait { polledModule.status.isIdle }
        polledModule.poll()
        await settle()
        api.complete(api.requests.count - 1, json: #"{"generation":100,"source":"module","call":{"state":"active","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        await wait { polledModule.endpoint == "module-vowifi" && polledModule.status.isActive }
        polledModule.reset()
        print("PASS: first module poll binds endpoint owner; later VoWiFi cannot steal it before module terminal idle")

        let ownerRuntime = CallRuntimeStore(
            backend: BackendProcess(), defaults: isolatedDefaults("owner"))
        ownerRuntime.selectEndpoint("reader")
        ownerRuntime.readerAvailable = { true }
        ownerRuntime.poll()
        await settle()
        let ownerStatus = api.requests.count - 1
        api.complete(ownerStatus, json: #"{"generation":77,"source":"reader","call":{"state":"active","peer":"+85256600731","media_ready":true,"early_media":false,"signaling_ready":true}}"#)
        await wait { ownerRuntime.status.isActive }
        for _ in 0..<3 {
            ownerRuntime.poll()
            await settle()
            api.fail(api.requests.count - 1)
            await settle()
        }
        ownerRuntime.selectEndpoint("module")
        precondition(ownerRuntime.endpoint == "reader" && ownerRuntime.status.isActive,
                     "failed observations or UI selection must not move an active Rust call to another owner")
        ownerRuntime.hangup()
        await settle()
        let hangup = api.requests.count - 1
        let hangupBody = try JSONSerialization.jsonObject(
            with: api.requests[hangup].body ?? Data()) as! [String: Any]
        precondition(api.requests[hangup].path == "api/call/hangup"
                     && hangupBody["observed_owner"] as? String == "reader"
                     && hangupBody["generation"] as? Int == 77,
                     "hangup lost the Rust reader owner receipt after failed observations")
        api.complete(hangup, json: #"{"generation":77,"source":"reader","call":{"state":"ended","media_ready":false,"early_media":false,"signaling_ready":true}}"#)
        await settle()
        api.finishPending()
        await settle()

        let suiteName = "switch.call-receipt-fixture.\(UUID().uuidString)"
        let durableDefaults = UserDefaults(suiteName: suiteName)!
        durableDefaults.removePersistentDomain(forName: suiteName)
        let beforeDurable = api.requests.count
        let firstGUI = CallRuntimeStore(backend: BackendProcess(), defaults: durableDefaults)
        firstGUI.moduleCallModeReady = { true }
        firstGUI.dial("15550001111")
        await settle()
        let firstSubmit = api.requests.count - 1
        precondition(firstSubmit >= beforeDurable && api.requests[firstSubmit].path == "api/call/dial")
        let firstBody = api.requests[firstSubmit].body ?? Data()
        let firstJSON = try JSONSerialization.jsonObject(with: firstBody) as! [String: Any]
        let durableID = firstJSON["client_request_id"] as? String
        precondition(durableID?.isEmpty == false, "dial did not carry a client request identity")
        api.failUnknown(firstSubmit)
        await settle()
        precondition(firstGUI.dialInFlight && firstGUI.commandInFlight
                     && firstGUI.error?.contains("原请求编号") == true,
                     "unknown dial response released the durable fence")
        let fencedCount = api.requests.count
        firstGUI.dial("15550002222")
        firstGUI.reset()
        await settle()
        precondition(api.requests.count == fencedCount && firstGUI.dialInFlight,
                     "reset or a new dial replaced an unresolved request identity")

        let secondGUI = CallRuntimeStore(backend: BackendProcess(), defaults: durableDefaults)
        secondGUI.moduleCallModeReady = { true }
        precondition(secondGUI.dialInFlight && secondGUI.commandInFlight,
                     "GUI restart did not restore the unknown fence")
        secondGUI.poll()
        await settle()
        let replaySubmit = api.requests.indices.last {
            $0 >= fencedCount && api.requests[$0].path == "api/call/dial"
        }!
        precondition(api.requests[replaySubmit].body == firstBody,
                     "GUI restart re-encoded or replaced the durable call payload")
        let replayJSON = try JSONSerialization.jsonObject(
            with: api.requests[replaySubmit].body ?? Data()) as! [String: Any]
        precondition(replayJSON["client_request_id"] as? String == durableID,
                     "GUI restart generated a new request identity")
        api.complete(replaySubmit, json: #"{"phase":"unknown","outcome":"unknown","selected_owner":"module","generation":0,"dispatch_count":0}"#)
        await settle()
        precondition(secondGUI.commandInFlight && secondGUI.dialInFlight,
                     "unbound admitted module receipt released the Swift fence")
        secondGUI.acceptModuleStatus(CallStatus(
            state: "incoming", number: "15550009999", incoming: true, active: false))
        secondGUI.selectEndpoint("reader")
        let beforeBlockedAction = api.requests.count
        secondGUI.answer()
        secondGUI.hangup()
        await settle()
        precondition(api.requests.count == beforeBlockedAction && secondGUI.commandInFlight,
                     "next incoming call or owner selection bypassed the unknown mutation fence")
        secondGUI.reset()
        secondGUI.poll()
        await settle()
        let terminalReplay = api.requests.indices.last {
            $0 > replaySubmit && api.requests[$0].path == "api/call/dial"
        }!
        precondition(api.requests[terminalReplay].body == firstBody,
                     "unknown admission recovery replaced the original exact payload")
        api.complete(terminalReplay, json: #"{"accepted":true}"#)
        await settle()
        precondition(!secondGUI.commandInFlight && secondGUI.status.isIdle,
                     "old terminal replay either retained its fence or regressed the live projection")
        api.finishPending()
        await settle()
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
        await settle()
        precondition(persistenceRuntime.commandInFlight
                     && persistenceRuntime.error?.contains("持久保存") == true,
                     "failed checkpoint save did not retain a visible sticky fence")
        persistenceRuntime.poll()
        await settle()
        api.finishPending()
        await settle()
        persistenceRuntime.reset()
        failedPersistence.synchronizeResult = true
        persistenceRuntime.poll()
        persistenceRuntime.dial("15550004444")
        await settle()
        precondition(
            api.requests[beforePersistenceFailure...].allSatisfy {
                !["api/call/dial", "api/call/answer", "api/call/hangup"].contains($0.path)
            },
            "save failure was bypassed by poll/reset/retry and uploaded a call mutation")
        api.finishPending()
        await settle()

        let nonDataCheckpoint = FaultingCallCheckpointDefaults()
        nonDataCheckpoint.values["switch.call.pendingMutation.v1"] = "wrong defaults type"
        let corruptRuntime = CallRuntimeStore(
            backend: BackendProcess(), defaults: nonDataCheckpoint)
        let beforeCorruptPoll = api.requests.count
        corruptRuntime.poll()
        corruptRuntime.dial("15550005555")
        await settle()
        precondition(corruptRuntime.commandInFlight
                     && corruptRuntime.error?.contains("损坏") == true
                     && api.requests[beforeCorruptPoll...].allSatisfy {
                         !["api/call/dial", "api/call/answer", "api/call/hangup"].contains($0.path)
                     },
                     "non-Data checkpoint was treated as missing or uploaded")
        api.finishPending()
        await settle()

        print("PASS production call duplicate fencing/readiness/reject recovery, durable same-ID GUI restart, sticky persistence/non-Data fences, unbound module unknown receipt/next-call fence, history degradation, diagnostic cancellation/partial preservation, network error/manual reboot, allowlisted export")
    }
}
