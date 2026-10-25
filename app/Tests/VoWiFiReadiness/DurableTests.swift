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

extension VoWiFiReadinessTests {
    static func stopTargetJSON(_ scope: String, owner: String = "original") -> String {
        #"{"scope":"\#(scope)","daemon_epoch":"daemon-1","owner_id":"\#(owner)","owner_generation":1}"#
    }
    static func stopControlPath(_ scope: String) -> String {
        "api/\(scope == "reader" ? "reader/vowifi" : "vowifi")/stop-control?scope=\(scope)&daemon_epoch=daemon-1&owner_id=original&owner_generation=1"
    }
    static func stopControlJSON(_ scope: String, owner: String = "original", joined: Bool = true) -> String {
        """
        {"stop_control":{"scope":"\(scope)","daemon_epoch":"daemon-1","owner_id":"\(owner)","owner_generation":1,
        "signal_state":"published","receipt_durability":"not_requested","owner_join":"\(joined ? "joined" : "pending")",
        "cleanup":"\(joined ? "confirmed" : "unconfirmed")","restart_effect":"none","poll_after_ms":\(joined ? 0 : 1500),"message":"fixture closed wording must not override fields"}}
        """
    }
    static func withTarget(_ json: String, scope: String, owner: String = "original") -> String {
        String(json.dropLast()) + ",\"stop_target\":" + stopTargetJSON(scope, owner: owner) + "}"
    }
    @MainActor static func targetedReader(root: URL) -> ReaderRuntimeStore {
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: root)
        let json = readerSnapshotJSON.replacingOccurrences(of: statusJSON(smsReady: true), with: withTarget(statusJSON(smsReady: true), scope: "reader"))
        let snapshot = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(json.utf8))
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation, autoConnect: false, instanceID: "rust-instance")
        return reader
    }
    @MainActor static func stopControlDTOSeparatesPendingFromCleanup() {
        let target = try! JSONDecoder().decode(VoWiFiStopTarget.self, from: Data(stopTargetJSON("reader").utf8))
        expect(target.valid(for: "reader") && !target.valid(for: "module"), "target scope is exact")
        let query = URLComponents(string: "fixture://local/?\(target.query)")!.queryItems!
        expect(query.first { $0.name == "owner_id" }?.value == "original", "target query roundtrips exactly")
        let invalid = VoWiFiStopTarget(scope: "reader", daemonEpoch: "daemon-1", ownerID: "owner:1", ownerGeneration: 1)
        expect(!invalid.valid(for: "reader"), "both Rust and Swift reject colon/noncanonical opaque IDs")
        let pending = try! JSONDecoder().decode(VoWiFiStopControlResponse.self, from: Data(stopControlJSON("reader", joined: false).utf8)).stopControl
        expect(pending.matches(target) && !pending.finished && pending.displayMessage.contains("尚未确认"), "published pending cannot present closed")
        expect(!pending.displayMessage.contains("closed"), "wire message cannot override authoritative pending facts")
        for wrong in [VoWiFiStopTarget(scope: "module", daemonEpoch: "daemon-1", ownerID: "original", ownerGeneration: 1),
                      VoWiFiStopTarget(scope: "reader", daemonEpoch: "successor-epoch", ownerID: "original", ownerGeneration: 1),
                      VoWiFiStopTarget(scope: "reader", daemonEpoch: "daemon-1", ownerID: "successor", ownerGeneration: 1),
                      VoWiFiStopTarget(scope: "reader", daemonEpoch: "daemon-1", ownerID: "original", ownerGeneration: 2)] {
            expect(!pending.matches(wrong), "temporary receipt cannot borrow another scope/epoch/owner/generation")
        }

    }
    @MainActor static func readerFreshEnableWithoutTargetNeverStopsPreviousOwner() async {
        let root = fixtureCheckpointRoot(); defer { try? FileManager.default.removeItem(at: root) }
        let reader = targetedReader(root: root) // Old authoritative owner A.
        let first = api.requests.count
        reader.enableVoWiFi()
        await waitForRequest(first, path: "api/reader/vowifi")
        let startRequestID = requestID(at: first)
        api.complete(first, json: "{\"operation\":\(operationJSON("fresh-new-owner", generation: 2, enable: true, phase: "submitted"))}")
        await waitForRequest(first + 1, path: "api/reader/vowifi?observe_operation=true")
        reader.disableVoWiFi()
        await waitForRequest(first + 2, path: "api/reader/vowifi")
        let stop = first + 2
        let journal = root.appendingPathComponent("vowifi-reader-gui-requests.json")
        let original = try! Data(contentsOf: journal)
        let stopRequestID = requestID(at: stop)
        api.fail(stop, error: APIError(code: "vowifi_request_ledger_full"))
        await waitForRequest(stop + 1, path: "api/reader/vowifi?observe_operation=true")
        expect(!api.requests.dropFirst(first).contains { $0.path.contains("stop-control") },
               "fresh operation without a target must fail closed instead of POSTing to old owner A")
        expect(api.requests[stop + 1].path.contains(stopRequestID),
               "ledger refusal still reconciles the exact durable stop request")
        expect(try! Data(contentsOf: journal) == original && reader.voWiFiActionInFlight,
               "missing current target preserves the durable checkpoint and action fence")
        api.complete(stop + 1, json: withOperation(#"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#,
            operationJSON("rejected-\(stopRequestID)", generation: 0, enable: false, terminal: true, phase: "rejected", outcome: "failed")))
        await waitForRequest(stop + 2, path: "api/reader/vowifi?observe_operation=true")
        expect(api.requests[stop + 2].path.contains(startRequestID),
               "stop terminal cannot discard the original start checkpoint")
        api.complete(stop + 2, json: withOperation(#"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#,
            operationJSON("fresh-new-owner", generation: 2, enable: true, terminal: true, phase: "cancelled", outcome: "cancelled")))
        await waitForRequest(stop + 3, path: "api/reader/vowifi")
        api.complete(stop + 3, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
        await waitUntil { !reader.voWiFiActionInFlight }
        reader.reset()
        api.fail(first + 1) // Release the canceled old start observer.
        for _ in 0..<30 { await Task.yield() }
        expect(!api.requests.dropFirst(first).contains { $0.path.contains("stop-control") },
               "late canceled start observation cannot send an old-target temporary POST")
    }
    @MainActor static func readerStopFallbackFreezesTargetAndNeverClearsCheckpoint() async {
        let root = fixtureCheckpointRoot(); defer { try? FileManager.default.removeItem(at: root) }
        let reader = targetedReader(root: root)
        var successes = 0; reader.onToast = { _, success, _ in if success { successes += 1 } }
        let first = api.requests.count
        reader.disableVoWiFi()
        await waitForRequest(first, path: "api/reader/vowifi")
        let journal = root.appendingPathComponent("vowifi-reader-gui-requests.json")
        let original = try! Data(contentsOf: journal)
        let requestID = requestID(at: first)
        // A fresh status can describe a successor, but cannot replace the
        // action token captured before the original failed mutation.
        let successorJSON = readerSnapshotJSON.replacingOccurrences(of: statusJSON(smsReady: true), with: withTarget(statusJSON(smsReady: true), scope: "reader", owner: "successor"))
        let successor = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(successorJSON.utf8))
        reader.applySnapshot(successor, issues: [], expectedGeneration: reader.generation, autoConnect: false, instanceID: "rust-instance")
        api.fail(first, error: APIError(code: "vowifi_request_ledger_full"))
        await waitForRequest(first + 1, path: stopControlPath("reader"))
        expect(api.requests[first + 1].method == "POST" && api.requests[first + 1].body == Data(), "temporary POST carries only empty Data")
        expect(api.requests[first + 1].path.contains("owner_id=original") && !api.requests[first + 1].path.contains("successor"), "fallback never recaptures a successor")
        api.complete(first + 1, json: stopControlJSON("reader"))
        await waitForRequest(first + 2, path: "api/reader/vowifi?observe_operation=true")
        expect(api.requests[first + 2].path.contains(requestID), "durable reconciliation retains original ID")
        expect(try! Data(contentsOf: journal) == original, "temporary joined/cleanup must not change original journal bytes")
        expect(reader.voWiFiActionInFlight && successes == 0, "temporary cleanup never unlocks original request or emits durable success")
        // Only this explicit authoritative durable terminal receipt may
        // settle the unchanged original checkpoint after the assertions.
        api.complete(first + 2, json: withOperation(#"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#, operationJSON("rejected-\(requestID)", generation: 0, enable: false, terminal: true, phase: "rejected", outcome: "failed")))
        await waitForRequest(first + 3, path: "api/reader/vowifi")
        api.complete(first + 3, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
        await waitUntil { !reader.voWiFiActionInFlight }
        reader.reset()
    }
    @MainActor static func checkpointWriteFailureStillStopsCachedReaderAndModule() async {
        for scope in ["reader", "module"] {
            let root = fixtureCheckpointRoot(); defer { try? FileManager.default.removeItem(at: root) }
            let journal = root.appendingPathComponent("vowifi-\(scope)-gui-requests.json")
            let first = api.requests.count
            if scope == "reader" {
                let reader = targetedReader(root: root)
                try! FileManager.default.createDirectory(at: journal, withIntermediateDirectories: true)
                reader.disableVoWiFi()
                await waitForRequest(first, path: stopControlPath("reader"))
                expect(api.requests[first].body == Data(), "reader journal failure still sends empty exact-target POST")
                api.complete(first, json: stopControlJSON("reader"))
                await waitUntil { reader.error?.contains("临时停止") == true }
                expect(reader.voWiFiActionInFlight && FileManager.default.fileExists(atPath: journal.path), "temporary result preserves failed journal fence")
                reader.reset()
            } else {
                let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: root)
                await waitForRequest(first, path: "api/vowifi")
                api.complete(first, json: withTarget(moduleStatusJSON, scope: "module"))
                await waitUntil { module.state.loaded }
                try! FileManager.default.createDirectory(at: journal, withIntermediateDirectories: true)
                module.disable()
                await waitForRequest(first + 1, path: stopControlPath("module"))
                expect(api.requests[first + 1].body == Data(), "module journal failure still sends empty exact-target POST")
                api.complete(first + 1, json: stopControlJSON("module"))
                await waitUntil { module.state.error?.contains("临时停止") == true }
                expect(module.state.actionInFlight && FileManager.default.fileExists(atPath: journal.path), "module temporary result never clears failed checkpoint")
                module.reset()
            }
        }
    }
    @MainActor static func moduleUnknownStopReplyUsesOriginalTargetAndPreservesCheckpoint() async {
        let root = fixtureCheckpointRoot(); defer { try? FileManager.default.removeItem(at: root) }
        let module = ModuleVoWiFiStore(backend: BackendProcess(), checkpointRoot: root)
        let first = api.requests.count
        await waitForRequest(first, path: "api/vowifi")
        api.complete(first, json: withTarget(moduleStatusJSON, scope: "module"))
        await waitUntil { module.state.loaded }
        module.disable()
        await waitForRequest(first + 1, path: "api/vowifi")
        let journal = root.appendingPathComponent("vowifi-module-gui-requests.json")
        let original = try! Data(contentsOf: journal)
        api.failUnknown(first + 1)
        await waitForRequest(first + 2, path: stopControlPath("module"))
        expect(api.requests[first + 2].path.contains("owner_id=original") && api.requests[first + 2].body == Data(), "unknown durable stop uses frozen target")
        api.complete(first + 2, json: stopControlJSON("module"))
        await waitForRequest(first + 3, path: "api/vowifi/result?observe_operation=true")
        expect(try! Data(contentsOf: journal) == original && module.state.actionInFlight, "module temporary cleanup never clears durable unknown")
        module.reset(); api.fail(first + 3)
        for _ in 0..<30 { await Task.yield() }
    }
}
