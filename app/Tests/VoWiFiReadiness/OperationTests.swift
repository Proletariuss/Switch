import Foundation

extension VoWiFiReadinessTests {
    static func operationJSON(_ id: String = "epoch-1-0", generation: UInt64 = 1,
                              enable: Bool = true, terminal: Bool = false,
                              success: Bool = false, expired: Bool = false,
                              cleanup: Bool = false, phase: String = "fixture",
                              outcome: String? = nil) -> String {
        """
        {"operation_id":"\(id)","generation":\(generation),"requested_enable":\(enable),
         "phase":"\(phase)",\(outcome.map { "\"outcome\":\"\($0)\"," } ?? "")"terminal":\(terminal),"succeeded":\(success),
         "observation_expired":\(expired),"cleanup_confirmed":\(cleanup),
         "poll_after_ms":0,"message":"owner receipt"}
        """
    }

    static func withOperation(_ json: String, _ operation: String) -> String {
        String(json.dropLast()) + ",\"operation\":" + operation + "}"
    }

    @MainActor static func requestID(at index: Int) -> String {
        let data = api.requests[index].body ?? Data()
        let value = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
        return value["client_request_id"] as! String
    }

    static let moduleStatusJSON = """
        {"state":"registered","summary":"fixture","airplane_mode":false,"cfun_mode":1,
         "usbnet_mode":1,"usbnet_ready":true,"ims_wifi_calling_enabled":true,
         "ims_call_mode_set":true,"wifi_calling_mode":1,"epdg_auto_detect":true,
         "ims_registered":true,"vowifi_active":true}
        """

    static let moduleDisconnectedStatusJSON = """
        {"state":"disabled","summary":"fixture","airplane_mode":false,"cfun_mode":1,
         "usbnet_mode":1,"usbnet_ready":true,"ims_wifi_calling_enabled":false,
         "ims_call_mode_set":false,"wifi_calling_mode":0,"epdg_auto_detect":true,
         "ims_registered":false,"vowifi_active":false,"sms_ready":false,"call_ready":false,
         "progress_step":0}
        """

    @MainActor static func disconnectedStore() -> ReaderRuntimeStore {
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        let json = """
            {"readers":["Fixture Reader"],"selected":"Fixture Reader",
             "info":\(readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")),
             "vowifi":{"reader_id":"Fixture Reader","reader_generation":1,
                        "sim_id":"synthetic-sim","available":true,
                        "tunnel_state":"disconnected","ims_state":"idle",
                        "sms_ready":false,"call_ready":false,"progress_step":0}}
            """
        let snapshot = try! JSONDecoder().decode(
            ReaderDeviceSnapshot.self, from: Data(json.utf8))
        reader.applySnapshot(
            snapshot, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.voWiFiStatus?.tunnelState == "disconnected",
               "fixture starts disconnected")
        return reader
    }

    @MainActor static func disconnectedOperationCompanionsNeverPublishReady() async {
        for mode in ["terminal", "expired", "superseded"] {
            let reader = disconnectedStore()
            let start = api.requests.count
            reader.enableVoWiFi()
            await waitForRequest(start, path: "api/reader/vowifi")
            api.complete(start, json: "{\"operation\":\(operationJSON())}")
            await waitUntil { api.requests.count > start + 1 }

            let operation: String
            switch mode {
            case "expired":
                operation = operationJSON(expired: true)
            case "superseded":
                operation = operationJSON(
                    terminal: true, phase: "superseded", outcome: "superseded")
            default:
                operation = operationJSON(
                    terminal: true, phase: "failed", outcome: "failed")
            }
            api.complete(
                start + 1,
                json: withOperation(statusJSON(smsReady: true), operation))
            for _ in 0..<50 { await Task.yield() }
            expect(reader.voWiFiStatus?.tunnelState != "connected"
                   && reader.voWiFiStatus?.smsReady != true
                   && reader.voWiFiStatus?.callReady != true,
                   "\(mode) receipt must reject a ready companion from a disconnected baseline")

            if mode == "expired" {
                expect(reader.voWiFiActionInFlight,
                       "expired observation keeps the exact Rust owner fenced")
                try? await Task.sleep(for: .milliseconds(1_600))
                await waitUntil { api.requests.count > start + 2 }
                api.complete(start + 2, json: withOperation(
                    #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#,
                    operationJSON(terminal: true, phase: "failed", outcome: "failed")))
            }
            let refreshIndex = mode == "expired" ? start + 3 : start + 2
            await waitUntil { api.requests.count > refreshIndex }
            expect(api.requests[refreshIndex].path == "api/reader/vowifi",
                   "terminal \(mode) receipt reconciles with a plain status read")
            api.complete(
                refreshIndex,
                json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle","sms_ready":false,"call_ready":false,"progress_step":0}"#)
            await waitUntil { !reader.voWiFiActionInFlight }
            expect(reader.voWiFiStatus?.tunnelState != "connected"
                   && reader.voWiFiStatus?.smsReady != true
                   && reader.voWiFiStatus?.callReady != true,
                   "\(mode) reconciliation remains disconnected")
        }

        for mode in ["terminal", "expired", "superseded"] {
            let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
            let initial = api.requests.count
            await waitForRequest(initial, path: "api/vowifi")
            api.complete(initial, json: moduleDisconnectedStatusJSON)
            await waitUntil { module.state.loaded }
            expect(module.state.status?.vowifiActive == false,
                   "module fixture starts disconnected")

            module.enable()
            await waitForRequest(initial + 1, path: "api/vowifi")
            api.complete(initial + 1, json: "{\"operation\":\(operationJSON())}")
            await waitUntil { api.requests.count > initial + 2 }

            let operation: String
            switch mode {
            case "expired":
                operation = operationJSON(expired: true)
            case "superseded":
                operation = operationJSON(
                    terminal: true, phase: "superseded", outcome: "superseded")
            default:
                operation = operationJSON(
                    terminal: true, phase: "failed", outcome: "failed")
            }
            api.complete(
                initial + 2,
                json: "{\"done\":true,\"status\":\(moduleStatusJSON),\"operation\":\(operation)}")
            for _ in 0..<50 { await Task.yield() }
            expect(module.state.status?.vowifiActive != true
                   && module.state.status?.imsRegistered != true
                   && module.state.status?.smsReady != true
                   && module.state.status?.callReady != true,
                   "module \(mode) receipt must reject a ready companion from a disconnected baseline")

            if mode == "expired" {
                expect(module.state.actionInFlight,
                       "module expired observation keeps the exact Rust owner fenced")
                try? await Task.sleep(for: .milliseconds(1_600))
                await waitUntil { api.requests.count > initial + 3 }
                let failed = operationJSON(
                    terminal: true, phase: "failed", outcome: "failed")
                api.complete(
                    initial + 3,
                    json: "{\"done\":true,\"operation\":\(failed)}")
            }
            let refreshIndex = mode == "expired" ? initial + 4 : initial + 3
            await waitUntil { api.requests.count > refreshIndex }
            expect(api.requests[refreshIndex].path == "api/vowifi",
                   "module terminal \(mode) receipt reconciles with a plain status read")
            api.complete(refreshIndex, json: moduleDisconnectedStatusJSON)
            await waitUntil { !module.state.actionInFlight }
            expect(module.state.status?.vowifiActive != true
                   && module.state.status?.imsRegistered != true
                   && module.state.status?.smsReady != true
                   && module.state.status?.callReady != true,
                   "module \(mode) reconciliation remains disconnected")
        }
    }

    @MainActor static func structuredOperationCompatibilityAndOwnerFencing() async {
        expect(try! JSONDecoder().decode(VoWiFiAccepted.self, from: Data("{}".utf8)).operation == nil,
               "an older service must decode without inventing an operation")
        expect(try! JSONDecoder().decode(VoWiFiResult.self, from: Data(#"{"done":true}"#.utf8)).operation == nil,
               "legacy module results remain decodable")
        let legacy = try! JSONEncoder().encode(ReaderVoWiFiRequest(enable: true, proxy: nil))
        expect(!String(decoding: legacy, as: UTF8.self).contains("observe_operation"),
               "legacy request wire must omit observation fields")

        for mode in ["missing", "stale-id", "stale-generation", "expiry"] {
            let reader = readyStore()
            var successes = 0
            reader.onToast = { _, success, _ in if success { successes += 1 } }
            let start = api.requests.count
            reader.enableVoWiFi()
            await waitForRequest(start, path: "api/reader/vowifi")
            if mode == "missing" {
                api.complete(start, json: "{\"fixture_missing_operation\":true}")
                await waitForRequest(start + 1, path: "api/reader/vowifi?observe_operation=true")
                expect(reader.voWiFiActionInFlight,
                       "missing accepted receipt cannot release the retained write fence")
                api.complete(start + 1, json: withOperation(statusJSON(smsReady: false), operationJSON()))
            } else {
                api.complete(start, json: "{\"operation\":\(operationJSON())}")
                await waitUntil { api.requests.count > start + 1 }
                expect(api.requests[start + 1].path.contains("observe_operation=true"), "Rust capability is used on reads")
                let observed = operationJSON(mode == "stale-id" ? "old" : "epoch-1-0",
                    generation: mode == "stale-generation" ? 99 : 1, expired: mode == "expiry")
                api.complete(start + 1, json: withOperation(statusJSON(smsReady: true), observed))
            }
            for _ in 0..<50 { await Task.yield() }
            expect(reader.voWiFiActionInFlight,
                   "\(mode) observation cannot release the retained Rust owner")
            if mode != "missing" {
                try? await Task.sleep(for: .milliseconds(1_600))
            }
            await waitUntil { api.requests.count > start + 2 }
            expect(api.requests[start + 2].path.contains("operation_id=epoch-1-0"),
                   "\(mode) must re-observe the same Rust owner")
            api.complete(start + 2, json: withOperation(
                #"{"available":true,"tunnel_state":"disconnected"}"#,
                operationJSON(terminal: true)))
            let refresh = start + 3
            await waitUntil { api.requests.count > refresh }
            expect(api.requests[refresh].path == "api/reader/vowifi",
                   "\(mode) terminal owner must reconcile with a plain status read; got \(api.requests[refresh].path)")
            api.complete(refresh, json: #"{"available":true,"tunnel_state":"disconnected"}"#)
            await waitUntil { !reader.voWiFiActionInFlight && reader.voWiFiStatus?.tunnelState == "disconnected" }
            expect(successes == 0, "IMS/tunnel readiness cannot synthesize success for \(mode)")
        }

        let reader = readyStore()
        var successes = 0
        reader.onToast = { _, success, _ in if success { successes += 1 } }
        let start = api.requests.count
        reader.enableVoWiFi()
        await waitForRequest(start, path: "api/reader/vowifi")
        api.complete(start, json: "{\"operation\":\(operationJSON())}")
        await waitUntil { api.requests.count > start + 1 }
        reader.disableVoWiFi()
        await waitForRequest(start + 2, path: "api/reader/vowifi")
        let originalStopID = requestID(at: start + 2)
        for _ in 0..<8 { reader.disableVoWiFi() }
        for _ in 0..<50 { await Task.yield() }
        expect(api.requests.count == start + 3,
               "repeated stop clicks before POST receipt must not submit new owners")
        expect(requestID(at: start + 2) == originalStopID,
               "the first stop checkpoint remains the authoritative identity")
        api.complete(start + 2, json: "{\"operation\":\(operationJSON("epoch-1-1", enable: false))}")
        await waitUntil { api.requests.count > start + 3 }
        api.complete(start + 1, json: withOperation(statusJSON(smsReady: true),
            operationJSON(terminal: true, success: true)))
        for _ in 0..<50 { await Task.yield() }
        expect(reader.voWiFiActionInFlight && successes == 0, "late start cannot complete a newer cancel")
        api.complete(start + 3, json: withOperation(#"{"available":true,"tunnel_state":"disconnected"}"#,
            operationJSON("epoch-1-1", enable: false, terminal: true, success: true, cleanup: true)))
        await waitForRequest(start + 4, path: "api/reader/vowifi?observe_operation=true")
        expect(api.requests[start + 4].path.contains(requestID(at: start)),
               "the cancelled start checkpoint is reconciled by its exact ID")
        api.complete(start + 4, json: withOperation(#"{"available":true,"tunnel_state":"disconnected"}"#,
            operationJSON(terminal: true, phase: "superseded", outcome: "superseded")))
        await waitForRequest(start + 5, path: "api/reader/vowifi")
        api.complete(start + 5, json: #"{"available":true,"tunnel_state":"disconnected"}"#)
        await waitUntil { !reader.voWiFiActionInFlight }
        expect(successes == 1, "cancel succeeds only with Rust cleanup receipt")

        await moduleStructuredOperationFencing()
        await unknownMutationReceiptRecoversRustOwner()
        print("PASS: optional operation decoder, legacy wire, expiry, cancel cleanup, stale ID/generation and late response fencing")
    }

    @MainActor static func unknownMutationReceiptRecoversRustOwner() async {
        let reader = readyStore()
        let readerStart = api.requests.count
        reader.enableVoWiFi()
        await waitForRequest(readerStart, path: "api/reader/vowifi")
        let readerRequestID = requestID(at: readerStart)
        api.failUnknown(readerStart)
        await waitForRequest(readerStart + 1, path: "api/reader/vowifi?observe_operation=true")
        expect(api.requests[readerStart + 1].path.contains("client_request_id=\(readerRequestID)"),
               "reader recovery must query the exact durable mutation ID")
        expect(reader.voWiFiActionInFlight,
               "lost reader mutation receipt must retain the write fence")
        api.complete(readerStart + 1, json: withOperation(
            statusJSON(smsReady: false), operationJSON()))
        await waitUntil { api.requests.count > readerStart + 2 }
        expect(api.requests[readerStart + 2].path.contains("operation_id=epoch-1-0"),
               "reader recovery must continue with the recovered owner identity")
        api.complete(readerStart + 2, json: withOperation(
            #"{"available":true,"tunnel_state":"disconnected"}"#,
            operationJSON(terminal: true)))
        await waitForRequest(readerStart + 3, path: "api/reader/vowifi")
        api.complete(readerStart + 3, json: #"{"available":true,"tunnel_state":"disconnected"}"#)
        await waitUntil { !reader.voWiFiActionInFlight }

        let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        let moduleStart = api.requests.count
        await waitForRequest(moduleStart, path: "api/vowifi")
        api.complete(moduleStart, json: moduleStatusJSON)
        await waitUntil { module.state.loaded }
        module.enable()
        await waitForRequest(moduleStart + 1, path: "api/vowifi")
        let moduleRequestID = requestID(at: moduleStart + 1)
        api.failUnknown(moduleStart + 1)
        await waitForRequest(moduleStart + 2, path: "api/vowifi/result?observe_operation=true")
        expect(api.requests[moduleStart + 2].path.contains("client_request_id=\(moduleRequestID)"),
               "module recovery must query the exact durable mutation ID")
        expect(module.state.actionInFlight,
               "lost module mutation receipt must retain the write fence")
        api.complete(moduleStart + 2, json: "{\"done\":false,\"operation\":\(operationJSON())}")
        await waitUntil { api.requests.count > moduleStart + 3 }
        api.complete(moduleStart + 3, json: "{\"done\":true,\"operation\":\(operationJSON(terminal: true))}")
        await waitForRequest(moduleStart + 4, path: "api/vowifi")
        api.complete(moduleStart + 4, json: moduleStatusJSON)
        await waitUntil { !module.state.actionInFlight }
    }

    @MainActor static func moduleStructuredOperationFencing() async {
        let shared = ModuleVoWiFiConnectionIntent.sharedProxyUsingAutomaticCarrierTemplate(
            proxyHost: " shared.proxy.example ",
            proxyPort: " 1080 ")
        expect(shared.proxyHost == " shared.proxy.example "
               && shared.proxyPort == " 1080 ",
               "module must preserve the raw shared proxy for Rust normalization")
        expect(shared.epdg.isEmpty && shared.epdgPortText.isEmpty,
               "module must resolve ePDG from the active carrier instead of inheriting a reader-card override")

        let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        module.connectionIntent = {
            ModuleVoWiFiConnectionIntent(
                proxyHost: "proxy.example.test", proxyPort: "1080",
                epdg: "epdg.example.test", epdgPortText: "4500")
        }
        var successes = 0
        module.onToast = { _, success, _ in if success { successes += 1 } }
        let initial = api.requests.count
        await waitForRequest(initial, path: "api/vowifi")
        api.complete(initial, json: moduleStatusJSON)
        await waitUntil { module.state.loaded }
        module.enable()
        await waitForRequest(initial + 1, path: "api/vowifi")
        let body = try! JSONSerialization.jsonObject(
            with: api.requests[initial + 1].body!) as! [String: Any]
        expect(body["proxy_host"] as? String == "proxy.example.test"
               && body["proxy_port"] as? String == "1080"
               && body["epdg"] as? String == "epdg.example.test"
               && body["epdg_port_text"] as? String == "4500",
               "module start transports the same raw connection fields as reader VoWiFi")
        api.complete(initial + 1, json: "{\"operation\":\(operationJSON())}")
        await waitUntil { api.requests.count > initial + 2 }
        expect(api.requests[initial + 2].path.contains("operation_id=epoch-1-0"), "module read binds Rust ID")
        api.complete(initial + 2, json: "{\"done\":true,\"status\":\(moduleStatusJSON),\"operation\":\(operationJSON(expired: true))}")
        for _ in 0..<50 { await Task.yield() }
        expect(module.state.actionInFlight && successes == 0 && module.state.error != nil,
               "module keeps the Rust owner fenced and ignores ready status on observation expiry")
        try? await Task.sleep(for: .milliseconds(1_600))
        await waitUntil { api.requests.count > initial + 3 }
        expect(api.requests[initial + 3].path.contains("operation_id=epoch-1-0"),
               "module re-observes the same expired owner")
        api.complete(initial + 3, json: "{\"done\":true,\"operation\":\(operationJSON(terminal: true))}")
        await waitForRequest(initial + 4, path: "api/vowifi")
        api.complete(initial + 4, json: moduleStatusJSON)
        await waitUntil { !module.state.actionInFlight }
        module.enable()
        await waitForRequest(initial + 5, path: "api/vowifi")
        api.complete(initial + 5, json: "{\"fixture_missing_operation\":true}")
        await waitForRequest(initial + 6, path: "api/vowifi/result?observe_operation=true")
        expect(module.state.actionInFlight,
               "missing module accepted receipt must retain the write fence")
        api.complete(initial + 6, json: "{\"done\":false,\"operation\":\(operationJSON())}")
        await waitUntil { api.requests.count > initial + 7 }
        api.complete(initial + 7, json: "{\"done\":true,\"operation\":\(operationJSON(terminal: true))}")
        await waitForRequest(initial + 8, path: "api/vowifi")
        api.complete(initial + 8, json: moduleStatusJSON)
        await waitUntil { !module.state.actionInFlight }
        expect(successes == 0 && module.state.error != nil, "old module service cannot invent operation completion")
    }
}
