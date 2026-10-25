import Foundation

extension VoWiFiReadinessTests {
    @MainActor static func readerLostMutationReplyReconcilesWithoutReplay() async {
        let reader = readyStore()
        let start = api.requests.count
        reader.enableVoWiFi()
        await waitForRequest(start, path: "api/reader/vowifi")
        let requestID = requestID(at: start)
        api.failUnknown(start)
        await waitForRequest(start + 1, path: "api/reader/vowifi?observe_operation=true")
        expect(api.requests[start + 1].path.contains(requestID),
               "lost mutation reply recovers by the original request ID")
        api.fail(start + 1, error: APIError(code: "vowifi_request_id_unknown"))
        await waitForRequest(start + 2, path: "api/reader/vowifi/reconcile")
        let reconciledID = (try! JSONSerialization.jsonObject(
            with: api.requests[start + 2].body!) as! [String: Any])["client_request_id"] as? String
        expect(reconciledID == requestID,
               "the late original POST and reconciliation remain bound to one ID")
        let tombstone = operationJSON("rejected-\(requestID)", generation: 0,
            terminal: true, phase: "rejected", outcome: "failed")
        api.complete(start + 2, json: "{\"operation\":\(tombstone)}")
        await waitForRequest(start + 3, path: "api/reader/vowifi")
        api.complete(start + 3, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
        await waitUntil { !reader.voWiFiActionInFlight }
        expect(api.requests.dropFirst(start).filter {
            $0.method == "POST" && $0.path == "api/reader/vowifi"
        }.count == 1, "a lost original reply cannot submit the VoWiFi mutation again")
        reader.reset()
    }

    @MainActor static func readerReconcileUsesExactTerminalOrContinuesOwnerPoll() async {
        for terminalAtReconcile in [true, false] {
            let root = fixtureCheckpointRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let checkpoints = VoWiFiRequestCheckpointStore(scope: "reader", root: root)
            let requestID = terminalAtReconcile ? "reconcile-rejected" : "reconcile-admitted"
            let request = ReaderVoWiFiRequest(enable: true, proxy: nil,
                proxyHost: "frozen.proxy.example", proxyPort: "1080",
                observeOperation: true, clientRequestID: requestID)
            let checkpoint = try! checkpoints.begin(id: requestID, enable: true, request: request)
            let start = api.requests.count
            let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: root)
            await waitUntil {
                api.requests.dropFirst(start).contains { $0.path.hasPrefix("api/reader/vowifi?") }
            }
            let lookup = api.requests.indices.dropFirst(start).first {
                api.requests[$0].path.hasPrefix("api/reader/vowifi?")
            }!
            api.fail(lookup, error: APIError(code: "vowifi_request_id_unknown"))
            await waitUntil {
                api.requests.dropFirst(lookup + 1).contains { $0.path == "api/reader/vowifi/reconcile" }
            }
            let reconcile = api.requests.indices.dropFirst(lookup + 1).first {
                api.requests[$0].path == "api/reader/vowifi/reconcile"
            }!
            expect(api.requests[reconcile].body == checkpoint.payload,
                   "reconciliation must send the frozen payload, including its original ID")
            let operation = terminalAtReconcile
                ? operationJSON("rejected-\(requestID)", generation: 0, terminal: true,
                    phase: "rejected", outcome: "failed")
                : operationJSON("admitted-\(requestID)", generation: 4, phase: "submitted")
            api.complete(reconcile, json: "{\"operation\":\(operation)}")
            if !terminalAtReconcile {
                await waitUntil {
                    api.requests.dropFirst(reconcile + 1).contains {
                        $0.path.hasPrefix("api/reader/vowifi?") && $0.path.contains("operation_id=admitted-")
                    }
                }
                let observation = api.requests.indices.dropFirst(reconcile + 1).first {
                    api.requests[$0].path.contains("operation_id=admitted-")
                }!
                expect(try! checkpoints.load() == checkpoint,
                       "an admitted receipt retains the checkpoint until terminal observation")
                api.complete(observation, json: withOperation(
                    #"{"available":true,"tunnel_state":"disconnected"}"#,
                    operationJSON("admitted-\(requestID)", generation: 4,
                        terminal: true, phase: "failed", outcome: "failed")))
            }
            await waitUntil { (try? checkpoints.load()) == nil }
            await waitUntil { api.requests.dropFirst(reconcile + 1).contains { $0.path == "api/reader/vowifi" } }
            let refresh = api.requests.indices.dropFirst(reconcile + 1).last {
                api.requests[$0].path == "api/reader/vowifi"
            }!
            api.complete(refresh, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
            await waitUntil { !reader.voWiFiActionInFlight }
            expect(api.requests.dropFirst(start).allSatisfy {
                $0.method == "GET" || $0.path == "api/reader/vowifi/reconcile"
            }, "reconciliation must never repeat the original mutation")
            reader.reset()
        }
    }

    @MainActor static func unknownRequestIDKeepsCheckpointWithoutClaimingOwner() async {
        for scope in ["reader", "module"] {
            let root = fixtureCheckpointRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let checkpoints = VoWiFiRequestCheckpointStore(scope: scope, root: root)
            let requestID = "missing-receipt-\(scope)"
            let request = ReaderVoWiFiRequest(enable: true, proxy: nil,
                observeOperation: true, clientRequestID: requestID)
            let checkpoint = try! checkpoints.begin(id: requestID, enable: true, request: request)
            let start = api.requests.count
            let reader = scope == "reader"
                ? ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: root) : nil
            let module = scope == "module"
                ? ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: root) : nil
            let prefix = scope == "reader" ? "api/reader/vowifi?" : "api/vowifi/result?"
            await waitUntil { api.requests.dropFirst(start).contains { $0.path.hasPrefix(prefix) } }
            let first = api.requests.indices.dropFirst(start).first { api.requests[$0].path.hasPrefix(prefix) }!
            expect(api.requests[first].path.contains(requestID) && api.requests[first].method == "GET",
                   "missing receipt recovery reads the exact saved ID")
            if scope == "module" {
                await waitUntil { api.requests.dropFirst(start).contains { $0.path == "api/vowifi" } }
                let refresh = api.requests.indices.dropFirst(start).first { api.requests[$0].path == "api/vowifi" }!
                api.complete(refresh, json: moduleDisconnectedStatusJSON)
            }

            api.fail(first, error: APIError(code: "vowifi_request_id_unknown"))
            let reconcilePath = scope == "reader" ? "api/reader/vowifi/reconcile" : "api/vowifi/reconcile"
            await waitUntil {
                api.requests.dropFirst(first + 1).contains { $0.path == reconcilePath }
            }
            let reconcile = api.requests.indices.dropFirst(first + 1).first {
                api.requests[$0].path == reconcilePath
            }!
            expect(api.requests[reconcile].method == "POST"
                   && api.requests[reconcile].body == checkpoint.payload,
                   "safety reconciliation uses the frozen original JSON bytes")
            api.fail(reconcile, error: APIError(code: "vowifi_reconcile_owner_unconfirmed"))
            await waitUntil {
                (reader?.error ?? module?.state.error)?.contains("后台资源仍待安全核对") == true
            }
            let message = reader?.error ?? module?.state.error ?? ""
            expect(message.contains("安全核对") && !message.contains("owner 仍在工作")
                   && !message.contains("owner 尚未确认结束"),
                   "unknown request ID must not assert an active Rust owner")
            expect(reader?.voWiFiActionInFlight ?? module!.state.actionInFlight,
                   "missing receipt retains the operation fence")
            expect(try! checkpoints.load() == checkpoint,
                   "missing receipt cannot clear the durable checkpoint")
            reader?.enableVoWiFi()
            module?.enable()
            expect(api.requests.dropFirst(start).allSatisfy {
                $0.method == "GET" || $0.path == reconcilePath
            }, "missing receipt cannot replay or admit another VoWiFi mutation")

            try? await Task.sleep(for: .milliseconds(1_600))
            await waitUntil {
                api.requests.indices.dropFirst(first + 1).contains { api.requests[$0].path.hasPrefix(prefix) }
            }
            let retry = api.requests.indices.dropFirst(first + 1).first { api.requests[$0].path.hasPrefix(prefix) }!
            expect(api.requests[retry].path.contains(requestID) && api.requests[retry].method == "GET",
                   "retry keeps the original request ID and does not resubmit")
            let terminal = operationJSON("original-owner", generation: 4, terminal: true,
                phase: "superseded", outcome: "superseded")
            api.complete(retry, json: scope == "reader"
                ? "{\"available\":true,\"tunnel_state\":\"disconnected\",\"operation\":\(terminal)}"
                : "{\"done\":true,\"operation\":\(terminal)}")
            await waitUntil { (try? checkpoints.load()) == nil }
            await waitUntil { api.requests.count > retry + 1 }
            let refresh = api.requests.count - 1
            api.complete(refresh, json: scope == "reader"
                ? #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#
                : moduleDisconnectedStatusJSON)
            await waitUntil { !(reader?.voWiFiActionInFlight ?? module!.state.actionInFlight) }
            expect(api.requests.dropFirst(start).allSatisfy {
                $0.method == "GET" || $0.path == reconcilePath
            }, "terminal recovery finishes without replaying the mutation")
            reader?.reset()
            module?.reset()
        }
    }

    @MainActor static func durableLedgerGUIRestartAndExactReceiptFencing() async {
        for scope in ["reader", "module"] {
            let root = fixtureCheckpointRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let checkpoints = VoWiFiRequestCheckpointStore(scope: scope, root: root)
            let requestID = "gui-original-\(scope)"
            let request = ReaderVoWiFiRequest(enable: true, proxy: nil, proxyHost: "raw-proxy",
                proxyPort: "1080", epdg: "raw.epdg", epdgPortText: "4500",
                observeOperation: true, clientRequestID: requestID)
            let original = try! checkpoints.begin(id: requestID, enable: true, request: request)
            let reopened = VoWiFiRequestCheckpointStore(scope: scope, root: root)
            expect(try! reopened.load() == original, "GUI reopen preserves original ID and exact bytes")
            let fields = try! JSONSerialization.jsonObject(with: original.payload) as! [String: Any]
            expect(fields["proxy_host"] as? String == "raw-proxy", "raw original payload survives settings changes")
            let index = api.requests.count
            var reader: ReaderRuntimeStore?
            var module: ModuleVoWiFiStore?
            if scope == "reader" {
                reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: root)
                reader!.proxyIP = "changed-after-restart"
            } else {
                module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: root)
            }
            await waitUntil { api.requests.count > index }
            let path = scope == "reader" ? "api/reader/vowifi?" : "api/vowifi/result?"
            await waitUntil { api.requests.dropFirst(index).contains(where: { $0.path.hasPrefix(path) }) }
            let recovery = api.requests.indices.dropFirst(index).first { api.requests[$0].path.hasPrefix(path) }!
            expect(api.requests[recovery].path.contains(requestID), "GUI restart queries only the saved exact ID")
            expect(api.requests[recovery].method == "GET", "GUI restart does not submit a successor")
            if scope == "module" {
                await waitUntil { api.requests.dropFirst(index).contains(where: { $0.path == "api/vowifi" }) }
                let refresh = api.requests.indices.dropFirst(index).first { api.requests[$0].path == "api/vowifi" }!
                api.complete(refresh, json: moduleDisconnectedStatusJSON)
            }
            // A different request/owner, even if ready and terminal, may not clear the fence.
            let wrong = operationJSON("other-owner", generation: 77, terminal: true, success: true)
                .replacingOccurrences(of: "\"operation_id\"", with: "\"client_request_id\":\"wrong-ID\",\"operation_id\"")
            let wrapper = scope == "reader" ? "{\"available\":true,\"tunnel_state\":\"disconnected\",\"operation\":\(wrong)}"
                : "{\"done\":true,\"operation\":\(wrong)}"
            api.complete(recovery, json: wrapper)
            for _ in 0..<30 { await Task.yield() }
            expect(reader?.voWiFiActionInFlight ?? module!.state.actionInFlight, "cross-owner terminal does not release saved fence")
            expect(try! checkpoints.load() == original, "wrong ID never deletes the checkpoint")
            // Wait for the exact recovery retry, then supply the old owner's explicit terminal receipt.
            try? await Task.sleep(for: .milliseconds(1_600))
            await waitUntil { api.requests.count > recovery + 1 }
            let next = api.requests.indices.dropFirst(recovery + 1).last { api.requests[$0].path.hasPrefix(path) }!
            let terminal = operationJSON("original-owner", generation: 4, terminal: true,
                phase: "superseded", outcome: "superseded")
            api.complete(next, json: scope == "reader" ? "{\"available\":true,\"tunnel_state\":\"disconnected\",\"operation\":\(terminal)}"
                : "{\"done\":true,\"operation\":\(terminal)}")
            await waitUntil { (try? checkpoints.load()) == nil }
            await waitUntil { api.requests.count > next + 1 }
            let final = api.requests.count - 1
            api.complete(final, json: scope == "reader"
                ? #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#
                : moduleDisconnectedStatusJSON)
            await waitUntil { !(reader?.voWiFiActionInFlight ?? module!.state.actionInFlight) }
            expect(api.requests.dropFirst(index).allSatisfy { $0.method == "GET" }, "dispatcher count after GUI restart is zero")
            expect(reader?.voWiFiStatus?.smsReady != true && module?.state.status?.vowifiActive != true,
                "old receipt never publishes successor readiness")
            reader?.reset()
            module?.reset()
        }
        // An unreadable checkpoint is never interpreted as an empty request set.
        let root = fixtureCheckpointRoot()
        try! FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try! Data("corrupt".utf8).write(to: root.appendingPathComponent("vowifi-reader-gui-requests.json"))
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: root)
        expect(reader.voWiFiActionInFlight, "corrupt checkpoint fails closed before dispatch")
        let before = api.requests.count
        reader.enableVoWiFi()
        expect(api.requests.count == before, "corrupt checkpoint cannot admit a successor")
        try? FileManager.default.removeItem(at: root)
    }

    @MainActor static func moduleReconcileRestoresOriginalCheckpointWithoutReplay() async {
        for mode in ["terminal", "admitted", "mismatched"] {
            let root = fixtureCheckpointRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let checkpoints = VoWiFiRequestCheckpointStore(scope: "module", root: root)
            let requestID = "module-reconcile-\(mode)"
            let request = ReaderVoWiFiRequest(enable: true, proxy: nil,
                proxyHost: "frozen.module.proxy", proxyPort: "1080",
                epdg: "frozen.module.epdg", epdgPortText: "4500",
                observeOperation: true, clientRequestID: requestID)
            let checkpoint = try! checkpoints.begin(id: requestID, enable: true, request: request)
            let start = api.requests.count
            let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: root)
            await waitUntil { api.requests.dropFirst(start).contains { $0.path == "api/vowifi" } }
            let initial = api.requests.indices.dropFirst(start).first { api.requests[$0].path == "api/vowifi" }!
            api.complete(initial, json: moduleDisconnectedStatusJSON)
            await waitUntil { api.requests.dropFirst(start).contains { $0.path.hasPrefix("api/vowifi/result?") } }
            let lookup = api.requests.indices.dropFirst(start).first { api.requests[$0].path.hasPrefix("api/vowifi/result?") }!
            api.fail(lookup, error: APIError(code: "vowifi_request_id_unknown"))
            await waitUntil { api.requests.dropFirst(lookup + 1).contains { $0.path == "api/vowifi/reconcile" } }
            let reconcile = api.requests.indices.dropFirst(lookup + 1).first { api.requests[$0].path == "api/vowifi/reconcile" }!
            expect(api.requests[reconcile].method == "POST" && api.requests[reconcile].body == checkpoint.payload,
                   "module reconciliation uploads the exact frozen fields and original ID")
            let operation = mode == "admitted"
                ? operationJSON("module-original", generation: 4, phase: "submitted")
                : operationJSON("rejected-\(requestID)", generation: 0, terminal: true, phase: "rejected", outcome: "failed")
            let reconciled = mode == "mismatched"
                ? operation.replacingOccurrences(of: "\"operation_id\"", with: "\"client_request_id\":\"another-request\",\"operation_id\"")
                : operation
            api.complete(reconcile, json: "{\"operation\":\(reconciled)}")
            if mode != "terminal" {
                if mode == "mismatched" {
                    await waitUntil { module.state.error?.contains("安全核对回执不匹配") == true }
                    expect(try! checkpoints.load() == checkpoint && module.state.actionInFlight,
                           "a different reconcile receipt cannot clear the module checkpoint")
                    try? await Task.sleep(for: .milliseconds(1_600))
                }
                await waitUntil { api.requests.dropFirst(reconcile + 1).contains { $0.path.hasPrefix("api/vowifi/result?") } }
                let observation = api.requests.indices.dropFirst(reconcile + 1).first { api.requests[$0].path.hasPrefix("api/vowifi/result?") }!
                expect(api.requests[observation].path.contains(requestID), "module keeps observing its original ID")
                expect(try! checkpoints.load() == checkpoint, "nonterminal module receipt retains the checkpoint")
                api.complete(observation, json: "{\"done\":true,\"operation\":\(operationJSON("module-original", generation: 4, terminal: true, phase: "failed", outcome: "failed"))}")
            }
            await waitUntil { (try? checkpoints.load()) == nil }
            await waitUntil { api.requests.dropFirst(reconcile + 1).contains { $0.path == "api/vowifi" } }
            let refresh = api.requests.indices.dropFirst(reconcile + 1).last { api.requests[$0].path == "api/vowifi" }!
            api.complete(refresh, json: moduleDisconnectedStatusJSON)
            await waitUntil { !module.state.actionInFlight }
            expect(api.requests.dropFirst(start).allSatisfy { $0.method == "GET" || $0.path == "api/vowifi/reconcile" },
                   "GUI restart reconciliation cannot repeat the module mutation")
            module.reset()
        }

        let root = fixtureCheckpointRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: root)
        let initial = api.requests.count
        await waitForRequest(initial, path: "api/vowifi")
        api.complete(initial, json: moduleDisconnectedStatusJSON)
        await waitUntil { module.state.loaded }
        let start = api.requests.count
        module.enable()
        await waitForRequest(start, path: "api/vowifi")
        let originalID = requestID(at: start)
        let checkpoints = VoWiFiRequestCheckpointStore(scope: "module", root: root)
        let original = try! checkpoints.load()!
        api.failUnknown(start)
        await waitForRequest(start + 1, path: "api/vowifi/result?observe_operation=true")
        api.fail(start + 1, error: APIError(code: "vowifi_request_id_unknown"))
        await waitForRequest(start + 2, path: "api/vowifi/reconcile")
        expect(api.requests[start + 2].body == original.payload && original.clientRequestID == originalID,
               "lost module mutation response uses its original checkpoint bytes")
        api.complete(start + 2, json: "{\"operation\":\(operationJSON("rejected-\(originalID)", generation: 0, terminal: true, phase: "rejected", outcome: "failed"))}")
        await waitForRequest(start + 3, path: "api/vowifi")
        api.complete(start + 3, json: moduleDisconnectedStatusJSON)
        await waitUntil { !module.state.actionInFlight }
        expect(api.requests.dropFirst(start).filter { $0.method == "POST" && $0.path == "api/vowifi" }.count == 1,
               "module lost response does not repeat the physical mutation")
        module.reset()
    }

}
