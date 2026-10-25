import Foundation

private final class FaultingReaderLifecycleDefaults: ReaderLifecycleCheckpointDefaults {
    var values: [String: Any] = [:]
    var synchronizeResult = true
    func object(forKey defaultName: String) -> Any? { values[defaultName] }
    func set(_ value: Any?, forKey defaultName: String) { values[defaultName] = value }
    func removeObject(forKey defaultName: String) { values.removeValue(forKey: defaultName) }
    func synchronize() -> Bool { synchronizeResult }
}

@main
struct VoWiFiReadinessTests {
    @MainActor static let api = ControlledAPI.shared

    @MainActor
    static func main() async {
        if CommandLine.arguments.contains("--durable-ledger-only") {
            await durableLedgerGUIRestartAndExactReceiptFencing()
            await readerLostMutationReplyReconcilesWithoutReplay()
            await readerReconcileUsesExactTerminalOrContinuesOwnerPoll()
            await moduleReconcileRestoresOriginalCheckpointWithoutReplay()
            await unknownRequestIDKeepsCheckpointWithoutClaimingOwner()
            expect(api.pendingCount == 0, "durable fixtures leave no unresolved continuations")
            print("PASS: durable VoWiFi checkpoint GUI restart, exact payload/ID, stale successor and cross-owner fencing")
            return
        }
        sameReaderIdentityGapPreservesOnlyCapabilities()
        await readerLifecycleCheckpointIsDurableAndSticky()
        presenceObservationSeparatesConnectionFromIdentity()
        await proxyCheckTransportsRawIntentAndSeparateRustFacts()
        await uncertainPresencePreservesOutstandingStop()
        await presenceRecoveryAndInterruptionFenceOutstandingStop()
        await recoveredIdentityWaitsForFreshAggregateAfterOldStopGET()
        await bleUnknownDiscoveryPreservesManualConnection()
        await failedIdentityObservationKeepsStopOwnership()
        await automaticReaderSelectionRequiresObservedCardEvidence()
        await listMissVerifiesRetainedOwnerBeforeAutomaticSelection()
        await lateListMissIdentityCannotUndoNewerProfile()
        await lateInventoryResultsAndFailuresCannotClearNewerProfile()
        await failedListRecoversRetainedBLEOwnerDuringModuleHang()
        await failedListRecoveryDoesNotDisturbOutstandingStop()
        await failedFallbackStatusPreservesNewlyVerifiedIdentity()
        await switchSynchronizationPreservesReaderCapabilities()
        await directStatusCannotRestoreReadinessWithoutIdentity()
        await explicitRustRefresh()
        await missingNativeSelectionVerifiesNativeOwnerBeforeAutoSwitch()
        await fallbackIdentityPrecedesStatusDuringModuleHang()
        await repeatedAggregateFailuresDoNotStarveIdentityRecovery()
        await fallbackIdentityReplyCannotUndoAnotherProfileSwitch()
        await fallbackStatusReplyCannotUndoAnotherProfileSwitch()
        await fallbackRejectsUnseenProfileChangeBetweenInfoAndStatus()
        await readerMutationBlocksVoWiFiRequests()
        await aggregateIdentityIssueFencesOldDirectGET()
        await newerAggregateStatusFencesOldDirectGET()
        await sameReaderSameSIMReconnectFencesOldGeneration()
        await disableWithdrawsReadinessAndRechecksRustStatus()
        await failedAggregateFallsBackToDirectStatus()
        await aggregateFailureDoesNotDisturbReaderAction()
        await repeatedDirectFailuresWithdrawOldReadiness()
        await lateDirectReplyCannotRestoreOldProfile()
        await lateDirectReplyCannotRestoreOldSelectedReader()
        await lateDirectReplyCannotRestoreOldReaderAfterListChange()
        await switchSyncStatusCannotRestoreNewerProfile()
        await switchSyncRefreshCannotReapplyOldProfile()
        sameSIMReaderIdentityChangeWithdrawsReadiness()
        await connectingSameSIMReaderWithdrawsOldReadiness()
        await disconnectingReaderWithdrawsOldReadiness()
        await failedReaderTransitionsRecheckStatus()
        await readerMutationsDoNotInterruptActiveVoWiFiAction()
        await lateDirectReplyCannotRestoreOldCard()
        progressStagesFollowActualReadiness()
        await structuredOperationCompatibilityAndOwnerFencing()
        await disconnectedOperationCompanionsNeverPublishReady()
        expect(api.pendingCount == 0, "all fixture requests completed")
        print("PASS: reader VoWiFi fallback, manual refresh, failure fencing, and five-stage presentation")
    }

    @MainActor static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }

    @MainActor static func waitUntil(
        _ condition: () -> Bool,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        for _ in 0..<1_000 {
            if condition() { return }
            await Task.yield()
        }
        fatalError("fixture did not reach expected state at \(file):\(line); paths=\(api.requests.map(\.path))")
    }

    @MainActor static func waitForRequest(_ index: Int, path: String) async {
        for _ in 0..<1_000 {
            if api.requests.count > index { break }
            await Task.yield()
        }
        expect(api.requests.count > index,
               "expected new \(path) request at \(index); paths=\(api.requests.map(\.path))")
        let actual = api.requests[index].path
        let exactRequestObservation = path.hasSuffix("observe_operation=true")
            && actual.hasPrefix(path + "&")
        expect(actual == path || exactRequestObservation,
               "expected \(path) at request \(index), got \(api.requests[index].path); tail=\(api.requests.suffix(8).map(\.path))")
    }

    @MainActor static func waitForPath(_ path: String, after index: Int) async -> Int {
        for _ in 0..<1_000 {
            if api.requests.dropFirst(index).contains(where: { $0.path == path }) {
                return api.requests.indices.first { $0 >= index && api.requests[$0].path == path }!
            }
            await Task.yield()
        }
        fatalError("fixture did not observe \(path) after \(index); paths=\(api.requests.map(\.path))")
    }

    static func statusJSON(
        smsReady: Bool,
        callReady: Bool = true,
        readerID: String = "Fixture Reader",
        simID: String = "synthetic-sim",
        readerGeneration: UInt64? = 1
    ) -> String {
        """
        {"reader_id":"\(readerID)","sim_id":"\(simID)","available":true,
         \(readerGeneration.map { "\"reader_generation\":\($0)," } ?? "")
         "tunnel_state":"connected","ims_state":"registered",
         "sms_ready":\(smsReady),"call_ready":\(callReady),"progress_step":5}
        """
    }

    static let readerSnapshotJSON = """
        {"readers":["Fixture Reader","Fixture Reader B"],"selected":"Fixture Reader",
         "inventory_revision":1,"reader_generation":1,
         "info":{"status":{"connected":true,"reader_name":"Fixture Reader",
                            "reader_generation":1,
                            "has_card":true,"has_usim_info":true,"has_esim":false},
                 "usim_info":{"iccid":"synthetic-sim","imsi":"synthetic-imsi",
                              "mcc":"001","mnc":"01"}},
        "vowifi":\(statusJSON(smsReady: true))}
        """

    static func readerInfoJSON(
        readerName: String,
        simID: String,
        imsi: String? = nil,
        readerGeneration: UInt64 = 1
    ) -> String {
        """
        {"status":{"connected":true,"reader_name":"\(readerName)",
                   "reader_generation":\(readerGeneration),
                   "has_card":true,"has_usim_info":true,"has_esim":false},
         "usim_info":{"iccid":"\(simID)","imsi":"\(imsi ?? "synthetic-imsi-\(simID)")",
                      "mcc":"001","mnc":"01"}}
        """
    }

    @MainActor static func readyStore() -> ReaderRuntimeStore {
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        let snapshot = try! JSONDecoder().decode(
            ReaderDeviceSnapshot.self, from: Data(readerSnapshotJSON.utf8))
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation,
                             autoConnect: false, instanceID: "rust-instance")
        expect(reader.voWiFiStatus?.smsReady == true, "fixture starts ready")
        return reader
    }

    @MainActor static func readerLifecycleCheckpointIsDurableAndSticky() async {
        let suite = "switch.reader.lifecycle.fixture.\(UUID().uuidString.lowercased())"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }
        let key = "reader-lifecycle-fixture"
        let first = ReaderLifecycleMutationCheckpointStore(defaults: defaults, key: key)
        let created = first.begin(
            action: "disconnect", reader: "Fixture Reader",
            readerGeneration: 7, inventoryRevision: 11,
            instanceID: "daemon-a", autoConnect: nil)!
        let restarted = ReaderLifecycleMutationCheckpointStore(
            defaults: UserDefaults(suiteName: suite)!, key: key)
        expect(restarted.begin(
            action: "disconnect", reader: "Fixture Reader",
            readerGeneration: 7, inventoryRevision: 11,
            instanceID: "daemon-a", autoConnect: nil) == created,
            "GUI restart did not preserve the exact reader lifecycle request ID")
        expect(restarted.begin(
            action: "disconnect", reader: "Successor Reader",
            readerGeneration: 8, inventoryRevision: 12,
            instanceID: "daemon-a", autoConnect: nil) == nil,
            "pending disconnect was rebound to a successor reader")
        expect(restarted.clear(created) && restarted.load() == .missing,
               "exact terminal lifecycle checkpoint did not clear")

        let faulting = FaultingReaderLifecycleDefaults()
        faulting.synchronizeResult = false
        let fenced = ReaderLifecycleMutationCheckpointStore(
            defaults: faulting, key: "reader-lifecycle-fault")
        expect(fenced.begin(
            action: "connect", reader: "Fixture Reader B",
            readerGeneration: 1, inventoryRevision: 1,
            instanceID: "rust-instance", autoConnect: nil) == nil,
            "failed lifecycle checkpoint persistence admitted a request")
        faulting.synchronizeResult = true
        expect(fenced.begin(
            action: "connect", reader: "Fixture Reader B",
            readerGeneration: 1, inventoryRevision: 1,
            instanceID: "rust-instance", autoConnect: nil) == nil,
            "later synchronize success bypassed the lifecycle persistence fence")

        let reader = ReaderRuntimeStore(
            backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot(),
            lifecycleCheckpoints: fenced)
        let snapshot = try! JSONDecoder().decode(
            ReaderDeviceSnapshot.self, from: Data(readerSnapshotJSON.utf8))
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation,
                             autoConnect: false, instanceID: "rust-instance")
        let count = api.requests.count
        reader.connect("Fixture Reader B")
        reader.reset()
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation,
                             autoConnect: false, instanceID: "rust-instance")
        reader.connect("Fixture Reader B")
        for _ in 0..<30 { await Task.yield() }
        expect(api.requests.count == count,
               "reset bypassed sticky lifecycle persistence failure and uploaded a mutation")

        let corruptDefaults = FaultingReaderLifecycleDefaults()
        corruptDefaults.values["reader-lifecycle-corrupt"] = "wrong type"
        let corrupt = ReaderLifecycleMutationCheckpointStore(
            defaults: corruptDefaults, key: "reader-lifecycle-corrupt")
        expect(corrupt.begin(
            action: "connect", reader: "Fixture Reader",
            readerGeneration: 1, inventoryRevision: 1,
            instanceID: "rust-instance", autoConnect: nil) == nil,
            "non-Data lifecycle evidence was overwritten")
    }

    @MainActor static func euiccStore() -> ReaderRuntimeStore {
        let reader = readyStore()
        let json = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")
            .replacingOccurrences(of: "\"has_esim\":false", with: "\"has_esim\":true,\"card_type\":\"euicc\"")
        reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(json.utf8)))
        reader.updateProfileOperatorName("Previous carrier")
        return reader
    }

    @MainActor static func proxyCheckTransportsRawIntentAndSeparateRustFacts() async {
        let json = readerSnapshotJSON
            .replacingOccurrences(
                of: "\"usim_info\":",
                with: "\"epdg_fqdn\":\"reader.example.test\",\"usim_info\":")
            .replacingOccurrences(
                of: "\"tunnel_state\":\"connected\"",
                with: "\"epdg_fqdn\":\"session.example.test\",\"tunnel_state\":\"connected\"")
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        let snapshot = try! JSONDecoder().decode(
            ReaderDeviceSnapshot.self, from: Data(json.utf8))
        reader.applySnapshot(
            snapshot, issues: [], expectedGeneration: reader.generation, autoConnect: false,
            instanceID: "rust-instance")
        expect(reader.epdgFQDN == "reader.example.test"
               && reader.voWiFiStatus?.epdgFqdn == "session.example.test",
               "fixture must expose both Rust-projected ePDG facts")
        reader.proxyIP = " raw.proxy.example "
        reader.proxyPort = " 1080 "
        reader.customEPDG = " "

        let index = api.requests.count
        reader.checkProxy()
        await waitForRequest(index, path: "api/reader/vowifi/proxy-check")
        let body = try! JSONSerialization.jsonObject(
            with: api.requests[index].body ?? Data()) as! [String: Any]
        expect(body["proxy_host"] as? String == " raw.proxy.example "
               && body["proxy_port"] as? String == " 1080 ",
               "Swift must transport raw proxy fields")
        expect(body["epdg"] as? String == " ",
               "Swift must transport the raw user ePDG override, including empty whitespace")
        expect(body["session_epdg"] as? String == "session.example.test"
               && body["reader_epdg"] as? String == "reader.example.test",
               "Swift must send separate display facts instead of selecting an effective ePDG")
        api.complete(index, json: #"{"ok":true,"effective_epdg":"session.example.test","epdg_source":"session_projection"}"#)
        await waitUntil { !reader.proxyChecking }
        expect(reader.proxyCheckResult?.effectiveEPDG == "session.example.test"
               && reader.proxyCheckResult?.epdgSource == "session_projection",
               "Swift must decode the structured Rust effective-ePDG decision")
    }

    @MainActor static func presenceObservationSeparatesConnectionFromIdentity() {
        for observation in [
            #""card_state":"absent","metadata_ready":false"#,
            #""card_state":"present","card_muted":true,"metadata_ready":false"#,
            #""card_state":"unavailable","metadata_ready":false"#,
            #""card_state":"unknown","presence_error":"scan_timeout","metadata_ready":false"#,
            #""card_state":"present","card_muted":false,"metadata_ready":false"#
        ] {
            let reader = euiccStore()
            // Deliberately include stale identity and ePDG to exercise the
            // client boundary, rather than relying on server masking alone.
            let json = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")
                .replacingOccurrences(of: "\"has_esim\":false", with: "\"has_esim\":false,\(observation)")
                .replacingOccurrences(of: "\"usim_info\":", with: "\"epdg_fqdn\":\"old.example\",\"usim_info\":")
            reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(json.utf8)))
            expect(reader.connected && reader.name == "Fixture Reader", "presence uncertainty must not deselect the reader")
            expect(reader.usimInfo == nil && reader.profileOperatorName == nil && reader.epdgFQDN == nil,
                   "unverified identity must not retain SIM, phone/operator or ePDG")
            expect(reader.voWiFiStatus == nil && reader.error != nil, "stale readiness must be withdrawn with a fixed diagnostic")
        }

        let ble = readyStore()
        let healthyBLE = readerInfoJSON(readerName: "BLE:12345678-1234-1234-1234-123456789ABC", simID: "verified-ble-sim")
            .replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"presence_source\":\"ble-bridge\",\"card_ready\":false,\"metadata_ready\":true")
            .replacingOccurrences(of: "\"has_esim\":false", with: "\"has_esim\":true,\"card_type\":\"euicc\"")
        ble.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(healthyBLE.utf8)))
        expect(ble.usimInfo?.iccid == "verified-ble-sim" && ble.hasESIM && ble.error == nil,
               "a verified BLE identity must remain usable without fabricating PCSC card presence")

        let noActiveProfile = euiccStore()
        let euiccOnly = #"{"status":{"connected":true,"reader_name":"Fixture Reader","reader_generation":1,"has_card":true,"card_state":"present","card_muted":false,"card_ready":true,"metadata_ready":false,"has_usim_info":false,"has_esim":true,"card_type":"euicc"}}"#
        noActiveProfile.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(euiccOnly.utf8)))
        expect(noActiveProfile.hasESIM && noActiveProfile.usimInfo == nil,
               "an eUICC without an active USIM must retain profile management")

        let unpowered = euiccStore()
        let idlePower = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")
            .replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":true,\"card_state\":\"present\",\"card_unpowered\":true,\"card_ready\":false,\"metadata_ready\":true")
        unpowered.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(idlePower.utf8)))
        expect(unpowered.usimInfo?.iccid == "synthetic-sim" && unpowered.error != nil,
               "power state alone must not assert a replaced SIM identity")
    }

    @MainActor static func uncertainPresencePreservesOutstandingStop() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.disableVoWiFi()
        await waitForRequest(index, path: "api/reader/vowifi")
        let unknown = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")
            .replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"presence_error\":\"scan_timeout\",\"metadata_ready\":false")
        reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(unknown.utf8)))
        expect(reader.voWiFiActionInFlight && reader.usimInfo == nil && reader.voWiFiStatus == nil,
               "an uncertain observation must withdraw identity without completing or abandoning the native stop")
        expect(api.requests.count == index + 1, "presence observation must not submit another hardware or VoWiFi command")
        api.complete(index, json: "{}")
        await waitForRequest(index + 1, path: "api/reader/vowifi")
        api.complete(index + 1, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle","sms_ready":false,"call_ready":false}"#)
        await waitUntil { !reader.voWiFiActionInFlight && reader.voWiFiLoaded }
        expect(reader.voWiFiStatus?.tunnelState == "disconnected" && reader.usimInfo == nil,
               "the real stop completion must still reconcile idle status without restoring old identity")
    }

    @MainActor static func presenceRecoveryAndInterruptionFenceOutstandingStop() async {
        for observation in ["same", "absent", "muted", "replacement"] {
            let reader = readyStore()
            let index = api.requests.count
            reader.disableVoWiFi()
            await waitForRequest(index, path: "api/reader/vowifi")
            let same = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim", imsi: "synthetic-imsi")
            let unknown = same.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"metadata_ready\":false")
            reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(unknown.utf8)))
            expect(reader.voWiFiActionInFlight && reader.usimInfo == nil, "uncertainty preserves operation ownership")
            let next: String
            switch observation {
            case "same": next = same.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":true,\"card_state\":\"present\",\"metadata_ready\":true")
            case "absent": next = same.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"absent\",\"metadata_ready\":false")
            case "muted": next = same.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":true,\"card_state\":\"present\",\"card_muted\":true,\"metadata_ready\":false")
            default: next = readerInfoJSON(readerName: "Fixture Reader", simID: "replacement-sim")
            }
            reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(next.utf8)))
            if observation == "same" {
                expect(reader.voWiFiActionInFlight && reader.usimInfo?.iccid == "synthetic-sim",
                       "recovery of the verified same SIM must keep the original stop owner")
                api.complete(index, json: "{}")
                await waitForRequest(index + 1, path: "api/reader/vowifi")
                api.complete(index + 1, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle","sms_ready":false,"call_ready":false}"#)
                await waitUntil { !reader.voWiFiActionInFlight && reader.voWiFiLoaded }
                expect(reader.voWiFiStatus?.tunnelState == "disconnected", "same-card recovery still reconciles the real stop result")
            } else {
                expect(reader.voWiFiActionInFlight && reader.voWiFiStatus == nil,
                       "confirmed interruption withdraws display but retains the durable request fence")
                api.complete(index, json: "{}")
                await waitForRequest(index + 1, path: "api/reader/vowifi?observe_operation=true")
                expect(api.requests[index + 1].path.contains(requestID(at: index)),
                       "replacement card recovery must query the original request ID")
                api.complete(index + 1, json: withOperation(
                    #"{"available":true,"tunnel_state":"disconnected"}"#,
                    operationJSON("old-owner", enable: false, terminal: true,
                                  phase: "superseded", outcome: "superseded")))
                await waitForRequest(index + 2, path: "api/reader/vowifi")
                api.complete(index + 2, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
                await waitUntil { !reader.voWiFiActionInFlight }
                expect(reader.voWiFiStatus?.smsReady != true,
                       "old exact terminal receipt cannot publish successor readiness")
                if observation == "absent" || observation == "muted" {
                    let newStop = api.requests.count
                    reader.disableVoWiFi()
                    await waitForRequest(newStop, path: "api/reader/vowifi")
                    reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(next.utf8)))
                    reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(next.utf8)))
                    expect(reader.voWiFiActionInFlight, "repeated confirmed interruption must not abandon a later explicit stop")
                    api.complete(newStop, json: "{}")
                    await waitForRequest(newStop + 1, path: "api/reader/vowifi")
                    api.complete(newStop + 1, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle","sms_ready":false,"call_ready":false}"#)
                    await waitUntil { !reader.voWiFiActionInFlight && reader.voWiFiLoaded }
                    expect(reader.voWiFiStatus?.tunnelState == "disconnected", "a stop issued after interruption can still reconcile idle state")
                }
            }
        }
    }

    @MainActor static func recoveredIdentityWaitsForFreshAggregateAfterOldStopGET() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.disableVoWiFi()
        await waitForRequest(index, path: "api/reader/vowifi")
        api.complete(index, json: "{}")
        await waitForRequest(index + 1, path: "api/reader/vowifi")
        let same = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim", imsi: "synthetic-imsi")
        let unknown = same.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"metadata_ready\":false")
        reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(unknown.utf8)))
        reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(same.utf8)))
        api.complete(index + 1, json: statusJSON(smsReady: true))
        for _ in 0..<100 { await Task.yield() }
        expect(reader.usimInfo?.iccid == "synthetic-sim" && reader.voWiFiStatus == nil,
               "identity recovery cannot authorize a status GET started before the uncertain observation")
        let current = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(readerSnapshotJSON.utf8))
        reader.applySnapshot(current, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.voWiFiLoaded && reader.voWiFiStatus?.smsReady == true,
               "the next fresh aggregate must restore status without another start/stop command")
        expect(api.requests.count == index + 2, "recovery must not manufacture another native command")
    }

    @MainActor static func bleUnknownDiscoveryPreservesManualConnection() async {
        let name = "BLE:12345678-1234-1234-1234-123456789ABC"
        let bleDevice = #"{"name":"BLE:12345678-1234-1234-1234-123456789ABC","card_state":"unknown","in_use":false,"muted":false,"atr_hex":""}"#
        let snapshot = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data("""
            {"readers":["\(name)"],"inventory_revision":11,"reader_generation":2,"devices":[\(bleDevice)]}
            """.utf8))
        expect(snapshot.autoConnect == nil, "a connected BLE peripheral alone is not a Rust auto-connect proposal")
        let mixed = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data("""
            {"readers":["\(name)","PCSC"],"devices":[\(bleDevice),
             {"name":"PCSC","card_state":"present","in_use":false,"muted":false,"atr_hex":"3B00"}]}
            """.utf8))
        expect(mixed.autoConnect == nil, "even present card facts cannot remint a missing Rust proposal in Swift")
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        let index = api.requests.count
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation,
                             autoConnect: true, instanceID: "rust-instance")
        for _ in 0..<20 { await Task.yield() }
        expect(api.requests.count == index, "unknown BLE discovery must not submit an aggregate auto-connect")
        reader.connect(name)
        await waitForRequest(index, path: "api/reader/connect")
        let verified = readerInfoJSON(readerName: name, simID: "verified-ble-sim")
            .replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"metadata_ready\":true")
            .replacingOccurrences(of: "\"has_esim\":false", with: "\"has_esim\":true,\"card_type\":\"euicc\"")
        api.complete(index, json: verified)
        await waitUntil { !reader.actionInFlight }
        expect(reader.connected && reader.name == name && reader.hasESIM && reader.usimInfo?.iccid == "verified-ble-sim",
               "explicit BLE connect and physically verified identity must remain supported")
    }

    @MainActor static func failedIdentityObservationKeepsStopOwnership() async {
        for aggregate in [false, true] {
            let reader = euiccStore()
            let stop = api.requests.count
            reader.disableVoWiFi()
            await waitForRequest(stop, path: "api/reader/vowifi")
            if aggregate {
                let snapshot = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(readerSnapshotJSON.utf8))
                let issue = try! JSONDecoder().decode(DeviceSnapshotIssue.self, from: Data(
                    #"{"endpoint":"/api/reader/info","code":"reader_observation_timeout","message":"fixture timeout","retryable":true}"#.utf8))
                reader.applySnapshot(snapshot, issues: [issue], expectedGeneration: reader.generation, autoConnect: false)
            } else {
                reader.refreshInfo()
                await waitForRequest(stop + 1, path: "api/reader/info")
                api.fail(stop + 1)
                await waitUntil { !reader.actionInFlight }
            }
            expect(reader.connected && reader.hasESIM && reader.usimInfo == nil && reader.epdgFQDN == nil,
                   "failed information observation must keep only reader capabilities, never stale identity")
            expect(reader.voWiFiActionInFlight && reader.voWiFiStatus == nil,
                   "information transport or 503 failure must not abandon the outstanding stop")
            let same = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")
            reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(same.utf8)))
            expect(reader.voWiFiActionInFlight, "same verified identity recovery must retain the stop binding")
            let next = api.requests.count
            api.complete(stop, json: "{}")
            await waitForRequest(next, path: "api/reader/vowifi")
            api.complete(next, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
            await waitUntil { reader.voWiFiLoaded && !reader.voWiFiActionInFlight }
            expect(reader.voWiFiStatus?.tunnelState == "disconnected", "the owned stop must reconcile after information failure recovery")
        }
    }

    static func discoveryDeviceJSON(_ name: String, _ state: String, muted: Bool = false) -> String {
        #"{"name":"\#(name)","card_state":"\#(state)","in_use":false,"muted":\#(muted),"atr_hex":""}"#
    }

    @MainActor static func automaticReaderSelectionRequiresObservedCardEvidence() async {
        let present = discoveryDeviceJSON("Candidate", "present")
        let unknownBLE = discoveryDeviceJSON("BLE:fixture", "unknown")
        let cases: [([String], String?, String?)] = [
            (["Candidate"], nil, nil),
            (["Candidate"], "[]", nil),
            (["BLE:fixture"], "[\(unknownBLE)]", nil),
            (["Candidate"], "[\(discoveryDeviceJSON("Candidate", "absent"))]", nil),
            (["Candidate"], "[\(discoveryDeviceJSON("Candidate", "unavailable"))]", nil),
            (["Candidate"], "[\(discoveryDeviceJSON("Candidate", "present", muted: true))]", nil),
            (["Other"], "[\(present)]", nil),
            (["Candidate"], "[\(present),\(present)]", nil),
            (["Candidate", "Candidate"], "[\(present)]", nil),
            (["BLE:fixture", "Candidate"], "[\(unknownBLE),\(present)]", "Candidate")
        ]
        defer { api.readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"# }
        for (names, devices, expected) in cases {
            let namesJSON = String(data: try! JSONSerialization.data(withJSONObject: names), encoding: .utf8)!
            let proposal = expected.map { ",\"auto_connect\":{\"reader\":\"\($0)\",\"inventory_revision\":42,\"reader_generation\":7,\"instance_id\":\"rust-instance\"}" } ?? ""
            let json = "{\"readers\":\(namesJSON),\"selected\":null\(devices.map { ",\"devices\":\($0)" } ?? "")\(proposal)}"
            let snapshot = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(json.utf8))
            expect(snapshot.autoConnect?.reader == expected,
                   "legacy card evidence alone cannot grant automatic selection without Rust's proposal")
            for aggregate in [false, true] {
                let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
                let index = api.requests.count
                if aggregate {
                    reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation, autoConnect: true)
                } else {
                    api.readerListJSON = json
                    reader.refreshList(autoConnect: true, refreshConnectedInfo: false)
                }
                if let expected {
                    await waitForRequest(index, path: "api/reader/connect")
                    let body = try! JSONSerialization.jsonObject(with: api.connectBodies.last!) as! [String: Any]
                    let proof = body["auto_connect"] as! [String: Any]
                    expect(proof["inventory_revision"] as? Int == 42 && proof["reader_generation"] as? Int == 7 && proof["instance_id"] as? String == "rust-instance",
                           "Swift forwards the exact inventory/generation/instance proposal without reminting it")
                    api.complete(index, json: readerInfoJSON(readerName: expected, simID: "verified-candidate"))
                    await waitUntil { !reader.actionInFlight && !reader.listRefreshInFlight }
                    expect(reader.name == expected && reader.usimInfo?.iccid == "verified-candidate",
                           "both native inventory paths retain normal positive auto-connect")
                } else {
                    await waitUntil { !reader.listRefreshInFlight }
                    for _ in 0..<20 { await Task.yield() }
                    expect(api.requests.count == index && !reader.actionInFlight,
                           "missing, conflicting or unusable card evidence must not send a connect command")
                }
            }
        }
    }

    @MainActor static func listMissVerifiesRetainedOwnerBeforeAutomaticSelection() async {
        defer { api.readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"# }
        let other = discoveryDeviceJSON("Other Reader", "present")
        api.readerListJSON = "{\"readers\":[\"Other Reader\"],\"selected\":null,\"devices\":[\(other)],\"auto_connect\":{\"reader\":\"Other Reader\",\"inventory_revision\":2,\"reader_generation\":2,\"instance_id\":\"rust-instance\"}}"
        for outcome in ["same", "unknown", "failed", "disconnected"] {
            let reader = euiccStore()
            let index = api.requests.count
            reader.refreshList(autoConnect: true, refreshConnectedInfo: false)
            await waitForRequest(index, path: "api/reader/info")
            expect(reader.connected && reader.name == "Fixture Reader" && reader.hasESIM,
                   "an inventory miss must retain the selected owner until native verification")
            expect(reader.usimInfo == nil && reader.voWiFiStatus == nil,
                   "the missing observation cannot leave old identity or readiness visible")
            if outcome == "failed" {
                api.fail(index)
            } else if outcome == "disconnected" {
                api.complete(index, json: #"{"status":{"connected":false,"has_card":false,"has_usim_info":false,"has_esim":false}}"#)
            } else {
                let same = readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim")
                let info = outcome == "unknown" ? same.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"metadata_ready\":false") : same
                api.complete(index, json: info)
            }
            if outcome == "disconnected" {
                await waitForRequest(index + 1, path: "api/reader/connect")
                api.complete(index + 1, json: readerInfoJSON(readerName: "Other Reader", simID: "other-sim", readerGeneration: 2))
                await waitUntil { !reader.actionInFlight && !reader.listRefreshInFlight }
                expect(reader.name == "Other Reader" && reader.usimInfo?.iccid == "other-sim",
                       "confirmed logical disconnection permits observed replacement auto-connect")
            } else {
                await waitUntil { !reader.listRefreshInFlight }
                expect(reader.connected && reader.name == "Fixture Reader" && api.requests.count == index + 1,
                       "same owner, uncertain identity or failed verification must not auto-switch readers")
                if outcome != "same" { expect(reader.usimInfo == nil && reader.voWiFiStatus == nil, "unknown or failed verification retains no old identity") }
            }
        }
    }

    @MainActor static func lateListMissIdentityCannotUndoNewerProfile() async {
        defer { api.readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"# }
        api.readerListJSON = #"{"readers":["Other Reader"],"selected":null,"devices":[]}"#
        let reader = readyStore()
        let index = api.requests.count
        reader.refreshList(autoConnect: true, refreshConnectedInfo: false)
        await waitForRequest(index, path: "api/reader/info")
        let replacement = readerInfoJSON(readerName: "Fixture Reader", simID: "newer-profile", readerGeneration: 2)
        reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(replacement.utf8)))
        api.complete(index, json: readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim", imsi: "synthetic-imsi"))
        await waitUntil { !reader.listRefreshInFlight }
        expect(reader.usimInfo?.iccid == "newer-profile" && reader.readerSessionGeneration == 2 && api.requests.count == index + 1,
               "late retained-owner verification cannot restore an older profile or select another reader")
    }

    @MainActor static func lateInventoryResultsAndFailuresCannotClearNewerProfile() async {
        defer {
            api.suspendReaderLists = false
            api.readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"#
        }
        for outcome in ["list-success", "list-failure", "info-failure"] {
            api.suspendReaderLists = outcome != "info-failure"
            api.readerListJSON = #"{"readers":["Other Reader"],"selected":null,"devices":[]}"#
            let reader = readyStore()
            let index = api.requests.count
            reader.refreshList(autoConnect: true, refreshConnectedInfo: false)
            await waitForRequest(index, path: outcome == "info-failure" ? "api/reader/info" : "api/reader/list")
            let info = readerInfoJSON(readerName: "Fixture Reader", simID: "newer-profile", readerGeneration: 2)
            let newer = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data("""
                {"readers":["Fixture Reader"],"selected":"Fixture Reader",
                 "info":\(info),"vowifi":\(statusJSON(smsReady: true, simID: "newer-profile", readerGeneration: 2))}
                """.utf8))
            reader.applySnapshot(newer, issues: [], expectedGeneration: reader.generation, autoConnect: false)
            expect(reader.voWiFiStatus?.smsReady == true, "new profile starts with current verified readiness")
            if outcome == "list-success" {
                api.complete(index, json: api.readerListJSON)
            } else {
                api.fail(index)
            }
            await waitUntil { !reader.listRefreshInFlight }
            expect(reader.usimInfo?.iccid == "newer-profile" && reader.readerSessionGeneration == 2 && reader.voWiFiStatus?.smsReady == true,
                   "late inventory or retained-owner failure must not clear a newer same-local-generation profile")
            expect(api.requests.count == index + 1, "a stale list cannot initiate a new identity read or automatic connection")
        }
    }

    @MainActor static func failedListRecoversRetainedBLEOwnerDuringModuleHang() async {
        api.suspendReaderLists = true
        defer { api.suspendReaderLists = false }
        let name = "BLE:12345678-1234-1234-1234-123456789ABC"
        let info = readerInfoJSON(readerName: name, simID: "verified-ble-sim")
            .replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false,\"card_state\":\"unknown\",\"metadata_ready\":true")
        let snapshot = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data("""
            {"readers":["\(name)"],"selected":"\(name)","info":\(info),
             "vowifi":\(statusJSON(smsReady: true, readerID: name, simID: "verified-ble-sim"))}
            """.utf8))
        let reader = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.refreshReaderFallback = { reader.refreshAfterAggregateFailure(autoConnect: true) }
        let index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)
        let list = await waitForPath("api/reader/list", after: index + 1)
        let health = await waitForPath("api/health", after: index + 1)
        api.fail(list)
        let identity = await waitForPath("api/reader/info", after: index + 1)
        expect(reader.usimInfo == nil && reader.voWiFiStatus == nil, "failed inventory must withdraw stale identity before direct owner verification")
        api.complete(identity, json: info)
        let status = await waitForPath("api/reader/vowifi", after: identity + 1)
        api.complete(status, json: statusJSON(smsReady: true, readerID: name, simID: "verified-ble-sim"))
        await waitUntil { !reader.fallbackRecoveryInFlight && reader.voWiFiStatus?.smsReady == true }
        expect(reader.name == name && reader.usimInfo?.iccid == "verified-ble-sim" && api.pendingCount == 1,
               "PCSC inventory failure and module hang cannot starve the independently verified BLE owner")
        expect(!api.requests.dropFirst(index).contains { $0.path == "api/reader/connect" }, "inventory failure recovery must not select another reader")
        api.complete(health, json: #"{"ok":true}"#)
        let module = await waitForPath("api/status", after: health + 1)
        api.complete(module, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
    }

    @MainActor static func failedListRecoveryDoesNotDisturbOutstandingStop() async {
        api.suspendReaderLists = true
        defer { api.suspendReaderLists = false }
        let reader = readyStore()
        let stop = api.requests.count
        reader.disableVoWiFi()
        await waitForRequest(stop, path: "api/reader/vowifi")
        reader.refreshList(autoConnect: true, refreshConnectedInfo: false)
        await waitForRequest(stop + 1, path: "api/reader/list")
        api.fail(stop + 1)
        await waitForRequest(stop + 2, path: "api/reader/info")
        api.complete(stop + 2, json: readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim", imsi: "synthetic-imsi"))
        await waitUntil { !reader.listRefreshInFlight }
        expect(reader.voWiFiActionInFlight && reader.voWiFiStatus == nil && api.requests.count == stop + 3,
               "inventory fallback must keep the original stop owner and never republish a competing ready status")
        api.complete(stop, json: "{}")
        await waitForRequest(stop + 3, path: "api/reader/vowifi")
        api.complete(stop + 3, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
        await waitUntil { !reader.voWiFiActionInFlight && reader.voWiFiLoaded }
        expect(reader.voWiFiStatus?.tunnelState == "disconnected", "the original stop completion must remain authoritative")

        let empty = ReaderRuntimeStore(backend: BackendProcess(), checkpointRoot: fixtureCheckpointRoot())
        let failedList = api.requests.count
        empty.refreshList(autoConnect: true)
        await waitForRequest(failedList, path: "api/reader/list")
        api.fail(failedList)
        await waitUntil { !empty.listRefreshInFlight }
        expect(!empty.connected && empty.error != nil && api.requests.count == failedList + 1,
               "a failed list without a retained owner must neither guess identity nor auto-connect")
    }

    @MainActor static func failedFallbackStatusPreservesNewlyVerifiedIdentity() async {
        for inventoryFails in [false, true] {
            api.suspendReaderLists = inventoryFails
            defer { api.suspendReaderLists = false }
            let reader = readyStore()
            let index = api.requests.count
            reader.refreshList(autoConnect: true, refreshConnectedInfo: false,
                               refreshVoWiFiAfterIdentity: true)
            if inventoryFails {
                await waitForRequest(index, path: "api/reader/list")
                api.fail(index)
            }
            let identity = await waitForPath("api/reader/info", after: index)
            let fresh = readerInfoJSON(readerName: "Fixture Reader", simID: "verified-profile-b", readerGeneration: 2)
                .replacingOccurrences(of: "\"usim_info\":", with: "\"epdg_fqdn\":\"fresh.example\",\"usim_info\":")
            api.complete(identity, json: fresh)
            let status = await waitForPath("api/reader/vowifi", after: identity + 1)
            expect(reader.usimInfo?.iccid == "verified-profile-b" && reader.epdgFQDN == "fresh.example",
                   "the current profile must be independently verified before the status request")
            api.fail(status)
            await waitUntil { !reader.listRefreshInFlight }
            expect(reader.usimInfo?.iccid == "verified-profile-b" && reader.readerSessionGeneration == 2 && reader.epdgFQDN == "fresh.example",
                   "a status failure cannot erase the newly verified profile identity or ePDG")
            expect(reader.voWiFiStatus == nil && reader.error != nil && reader.connected,
                   "failed status must withdraw readiness while retaining the verified connection")
        }
    }

    @MainActor static func sameReaderIdentityGapPreservesOnlyCapabilities() {
        let reader = euiccStore()
        let missing = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data("""
            {"readers":["Fixture Reader"],"selected":"Fixture Reader",
             "vowifi":\(statusJSON(smsReady: true))}
            """.utf8))
        reader.applySnapshot(missing, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.connected && reader.hasESIM && reader.cardType == "euicc",
               "same-reader missing snapshot must keep known eUICC presentation capability")
        expect(reader.usimInfo == nil && reader.profileOperatorName == nil && reader.epdgFQDN == nil,
               "pending identity must never retain old SIM fields")
        expect(reader.voWiFiStatus == nil, "missing identity must reject even an attached stale ready snapshot")

        let pending = #"{"status":{"connected":true,"reader_name":"Fixture Reader","reader_generation":2,"has_card":true,"has_usim_info":false,"has_esim":false}}"#
        reader.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(pending.utf8)))
        expect(reader.hasESIM && reader.cardType == "euicc" && reader.usimInfo == nil,
               "unresolved same-reader capability is a skeleton, not proof of SIM identity")
        for replacement in [
            pending.replacingOccurrences(of: "\"has_card\":true", with: "\"has_card\":false"),
            pending.replacingOccurrences(of: "\"has_esim\":false", with: "\"has_esim\":false,\"card_type\":\"physical_sim\""),
            readerInfoJSON(readerName: "Fixture Reader", simID: "replacement-physical-sim"),
            pending.replacingOccurrences(of: "Fixture Reader", with: "Another Reader")
        ] {
            let replaced = euiccStore()
            replaced.applyInfo(try! JSONDecoder().decode(ReaderInfoResponse.self, from: Data(replacement.utf8)))
            expect(!replaced.hasESIM && replaced.cardType != "euicc",
                   "confirmed card removal, physical SIM or reader change must retire old eUICC capability")
        }
        let other = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(
            #"{"readers":["Another Reader"],"selected":"Another Reader"}"#.utf8))
        reader.applySnapshot(other, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(!reader.hasESIM && reader.cardType == nil, "unidentified new reader cannot inherit old capabilities")
        reader.clearRuntimeState()
        expect(!reader.connected && reader.usimInfo == nil && !reader.hasESIM && reader.cardType == nil,
               "real disconnect clears all reader-backed presentation and identity")
    }

    @MainActor static func switchSynchronizationPreservesReaderCapabilities() async {
        let reader = euiccStore()
        let index = api.requests.count
        reader.synchronizeAfterProfileSwitch()
        expect(reader.hasESIM && reader.cardType == "euicc" && reader.connected,
               "profile confirmation gap must retain the same eUICC page skeleton")
        expect(reader.usimInfo == nil && reader.profileOperatorName == nil && reader.voWiFiStatus == nil,
               "profile confirmation gap must immediately withdraw old identity and service readiness")
        await waitForRequest(index, path: "api/reader/refresh")
        // A later real disconnect still owns the state even if the old refresh succeeds.
        reader.clearRuntimeState()
        api.complete(index, json: readerInfoJSON(readerName: "Fixture Reader", simID: "late-card"))
        for _ in 0..<20 { await Task.yield() }
        expect(!reader.connected && !reader.hasESIM && reader.usimInfo == nil,
               "old synchronization cannot restore a disconnected card")
        expect(api.requests.count == index + 1, "cancelled generation does not launch follow-up VoWiFi read")
    }

    @MainActor static func directStatusCannotRestoreReadinessWithoutIdentity() async {
        let reader = euiccStore()
        let missing = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(
            #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"#.utf8))
        reader.applySnapshot(missing, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.identityPending && reader.hasESIM, "fixture keeps capability, not SIM identity")
        let index = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(index, path: "api/reader/vowifi")
        api.complete(index, json: statusJSON(smsReady: true))
        await waitUntil { !reader.voWiFiRefreshInFlight }
        expect(reader.voWiFiStatus == nil && reader.usimInfo == nil && reader.hasESIM,
               "direct status refresh cannot restore ready state while USIM identity is absent")
        expect(reader.error?.contains("已拒绝显示就绪") == true,
               "direct identity mismatch remains explicit instead of presenting a false ready service")
    }

    @MainActor static func missingNativeSelectionVerifiesNativeOwnerBeforeAutoSwitch() async {
        let reader = readyStore()
        let firstRequest = api.requests.count
        api.readerListJSON = #"{"readers":["Fixture Reader","Fixture Reader B"],"selected":"Fixture Reader"}"#
        defer { api.readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"# }
        let missing = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(
            #"{"readers":["Fixture Reader B"],"selected":null}"#.utf8))

        reader.applySnapshot(missing, issues: [], expectedGeneration: reader.generation, autoConnect: true)
        expect(reader.connected && reader.name == "Fixture Reader",
               "one native inventory miss must retain the selected owner until verification")
        expect(reader.voWiFiStatus == nil, "unverified aggregate must withdraw old VoWiFi readiness")
        expect(!reader.actionInFlight, "one native inventory miss must not auto-connect Reader B")

        let infoIndex = await waitForPath("api/reader/info", after: firstRequest)
        expect(!api.requests.dropFirst(firstRequest).contains { $0.path == "api/reader/connect" },
               "native scan did not issue a competing reader selection")
        api.complete(infoIndex, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim", imsi: "synthetic-imsi"))
        let statusIndex = await waitForPath("api/reader/vowifi", after: infoIndex + 1)
        api.complete(statusIndex, json: statusJSON(smsReady: true))
        await waitUntil { !reader.fallbackRecoveryInFlight }
        expect(reader.connected && reader.name == "Fixture Reader",
               "native list recovery restored the original reader without switching")
        expect(!api.requests.dropFirst(firstRequest).contains { $0.path == "api/reader/connect" },
               "native identity recovery never auto-selected Reader B")
    }

    @MainActor static func explicitRustRefresh() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.refreshVoWiFi()
        expect(api.requests.count == index, "normal Rust snapshot polling does not add direct requests")
        // Explicit refresh must bypass only the normal Rust snapshot dedupe.
        reader.refreshVoWiFi(force: true)
        await waitForRequest(index, path: "api/reader/vowifi")
        api.complete(index, json: statusJSON(smsReady: false, callReady: false))
        await waitUntil { reader.voWiFiStatus?.smsReady == false }
        expect(reader.voWiFiStatus?.callReady == false, "manual refresh published degraded call state")
    }

    @MainActor static func disableWithdrawsReadinessAndRechecksRustStatus() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.disableVoWiFi()
        await waitForRequest(index, path: "api/reader/vowifi")
        expect(reader.voWiFiStatus == nil,
               "stop request must withdraw old ready display before backend teardown")
        let identityGap = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data("""
            {"readers":["Fixture Reader"],"selected":"Fixture Reader",
             "vowifi":\(statusJSON(smsReady: true))}
            """.utf8))
        reader.applySnapshot(identityGap, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.voWiFiActionInFlight && reader.voWiFiStatus == nil,
               "identity gap must withdraw readiness without abandoning an in-flight stop")
        api.complete(index, json: "{}")
        let statusIndex = await waitForPath("api/reader/vowifi", after: index + 1)
        api.complete(statusIndex, json: #"{"available":true,"tunnel_state":"disconnected","ims_state":"idle"}"#)
        await waitUntil { reader.voWiFiStatus?.tunnelState == "disconnected" }
        expect(!reader.voWiFiActionInFlight, "stop completed without leaving switch busy")
    }

    @MainActor static func newerAggregateStatusFencesOldDirectGET() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(index, path: "api/reader/vowifi")

        let newerJSON = readerSnapshotJSON.replacingOccurrences(
            of: statusJSON(smsReady: true), with: statusJSON(smsReady: false, callReady: false))
        let newer = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(newerJSON.utf8))
        reader.applySnapshot(newer, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.voWiFiStatus?.smsReady == false, "new aggregate status is published")
        api.complete(index, json: statusJSON(smsReady: true))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus?.smsReady == false && reader.voWiFiStatus?.callReady == false,
               "late direct response cannot override a newer same-card aggregate status")
    }

    @MainActor static func sameReaderSameSIMReconnectFencesOldGeneration() async {
        let reader = readyStore()
        let oldIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(oldIndex, path: "api/reader/vowifi")
        let reconnected = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(
                readerName: "Fixture Reader", simID: "synthetic-sim",
                imsi: "synthetic-imsi", readerGeneration: 2).utf8))
        reader.applyInfo(reconnected)
        expect(reader.voWiFiStatus == nil,
               "same-name same-SIM reader reconnect must withdraw old readiness")
        api.complete(oldIndex, json: statusJSON(smsReady: true, readerGeneration: 1))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil,
               "old generation direct GET cannot republish ready after reconnect")

        let missingIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(missingIndex, path: "api/reader/vowifi")
        api.complete(missingIndex, json: statusJSON(smsReady: true, readerGeneration: nil))
        await waitUntil { !reader.voWiFiRefreshInFlight }
        expect(reader.voWiFiStatus == nil,
               "live status without generation fails closed when current generation is known")

        let currentIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(currentIndex, path: "api/reader/vowifi")
        api.complete(currentIndex, json: statusJSON(smsReady: true, readerGeneration: 2))
        await waitUntil { reader.voWiFiStatus?.smsReady == true }
    }

    @MainActor static func aggregateIdentityIssueFencesOldDirectGET() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(index, path: "api/reader/vowifi")

        let snapshot = try! JSONDecoder().decode(
            ReaderDeviceSnapshot.self, from: Data(readerSnapshotJSON.utf8))
        let issue = DeviceSnapshotIssue(
            endpoint: "/api/reader/info", code: "upstream_snapshot_failed",
            message: "synthetic identity read failure", retryable: true)
        reader.applySnapshot(snapshot, issues: [issue],
                             expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.voWiFiStatus == nil, "identity issue withdraws readiness")
        api.complete(index, json: statusJSON(smsReady: true))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil,
               "late direct response cannot restore ready after aggregate identity issue")
    }

    @MainActor static func readerMutationBlocksVoWiFiRequests() async {
        for disconnecting in [false, true] {
            let reader = readyStore()
            let index = api.requests.count
            if disconnecting { reader.disconnect() } else { reader.connect("Fixture Reader B") }
            await waitForRequest(index, path: disconnecting ? "api/reader/disconnect" : "api/reader/connect")
            reader.refreshVoWiFi(force: true)
            reader.enableVoWiFi()
            reader.disableVoWiFi()
            expect(api.requests.count == index + 1,
                   "VoWiFi GET and mutations must wait until reader connect/disconnect completes")
            expect(!reader.voWiFiRefreshInFlight && !reader.voWiFiActionInFlight,
                   "blocked VoWiFi commands must not alter operation flags")
            if disconnecting {
                api.complete(index, json: "{}")
            } else {
                api.complete(index, json: readerInfoJSON(
                    readerName: "Fixture Reader B", simID: "synthetic-sim", imsi: "synthetic-imsi"))
            }
            await waitUntil { !reader.actionInFlight }
        }
    }

    @MainActor static func fallbackIdentityPrecedesStatusDuringModuleHang() async {
        let reader = readyStore()
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.readerActionInFlight = { reader.actionInFlight }
        gateway.refreshReaderFallback = {
            reader.refreshAfterAggregateFailure(autoConnect: false)
        }

        let index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)
        let healthIndex = await waitForPath("api/health", after: index + 1)
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil,
               "aggregate failure must withdraw old readiness before fallback I/O")
        let infoIndex = await waitForPath("api/reader/info", after: index + 1)
        expect(!api.requests.dropFirst(index + 1).contains { $0.path == "api/reader/vowifi" },
               "fallback must verify reader identity before requesting VoWiFi status")

        api.complete(infoIndex, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        let statusIndex = await waitForPath("api/reader/vowifi", after: index + 1)
        expect(reader.usimInfo?.iccid == "synthetic-sim-b",
               "same-reader eSIM switch must update identity before status becomes visible")
        api.complete(statusIndex, json: statusJSON(smsReady: true, simID: "synthetic-sim-b"))
        await waitUntil { reader.voWiFiStatus?.smsReady == true }
        expect(api.pendingCount == 1, "module health remains independent and pending")

        api.complete(healthIndex, json: #"{"ok":true}"#)
        let moduleStatusIndex = await waitForPath("api/status", after: healthIndex + 1)
        api.complete(moduleStatusIndex, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
    }

    @MainActor static func repeatedAggregateFailuresDoNotStarveIdentityRecovery() async {
        let reader = readyStore()
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.refreshReaderFallback = {
            reader.refreshAfterAggregateFailure(autoConnect: false)
        }

        let firstAggregate = api.requests.count
        gateway.refresh()
        await waitForRequest(firstAggregate, path: "api/v2/device-snapshot")
        api.fail(firstAggregate)
        let pendingInfo = await waitForPath("api/reader/info", after: firstAggregate + 1)
        let firstHealth = await waitForPath("api/health", after: firstAggregate + 1)
        expect(reader.fallbackRecoveryInFlight && reader.voWiFiStatus == nil,
               "first aggregate failure keeps one identity-bound recovery active")
        api.complete(firstHealth, json: #"{"ok":true}"#)
        let firstModuleStatus = await waitForPath("api/status", after: firstHealth + 1)
        api.complete(firstModuleStatus, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }

        // A periodic aggregate failure arrives before the first bounded
        // reader-info request has returned. It must neither cancel that
        // request nor start a second identity/status chain.
        let secondAggregate = api.requests.count
        for _ in 0..<1_000 where api.requests.count <= secondAggregate {
            gateway.refresh()
            await Task.yield()
        }
        await waitForRequest(secondAggregate, path: "api/v2/device-snapshot")
        api.fail(secondAggregate)
        let secondHealth = await waitForPath("api/health", after: secondAggregate + 1)
        for _ in 0..<30 { await Task.yield() }
        expect(reader.fallbackRecoveryInFlight && reader.voWiFiStatus == nil,
               "repeated aggregate failure preserves the first recovery and withdrawn readiness")
        expect(api.requests.dropFirst(pendingInfo + 1).filter { $0.path == "api/reader/info" }.isEmpty,
               "repeated aggregate failure must not restart the pending identity request")

        api.complete(pendingInfo, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        let recoveredStatus = await waitForPath("api/reader/vowifi", after: secondAggregate + 1)
        api.complete(recoveredStatus, json: statusJSON(smsReady: true, simID: "synthetic-sim-b"))
        await waitUntil { reader.voWiFiStatus?.smsReady == true && !reader.fallbackRecoveryInFlight }
        expect(reader.usimInfo?.iccid == "synthetic-sim-b",
               "first recovery finishes with status bound to the newly read identity")

        api.complete(secondHealth, json: #"{"ok":true}"#)
        let secondModuleStatus = await waitForPath("api/status", after: secondHealth + 1)
        api.complete(secondModuleStatus, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
    }

    @MainActor static func fallbackIdentityReplyCannotUndoAnotherProfileSwitch() async {
        let reader = readyStore()
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.refreshReaderFallback = { reader.refreshAfterAggregateFailure(autoConnect: false) }

        let index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)
        let infoIndex = await waitForPath("api/reader/info", after: index + 1)
        let healthIndex = await waitForPath("api/health", after: index + 1)
        let newer = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-c").utf8))
        reader.applyInfo(newer)
        api.complete(infoIndex, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.usimInfo?.iccid == "synthetic-sim-c" && reader.voWiFiStatus == nil,
               "fallback identity reply cannot restore a profile changed during recovery")
        expect(!api.requests.dropFirst(index + 1).contains { $0.path == "api/reader/vowifi" },
               "stale identity reply cannot start a VoWiFi status read")

        api.complete(healthIndex, json: #"{"ok":true}"#)
        let moduleStatusIndex = await waitForPath("api/status", after: healthIndex + 1)
        api.complete(moduleStatusIndex, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
    }

    @MainActor static func fallbackStatusReplyCannotUndoAnotherProfileSwitch() async {
        let reader = readyStore()
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.refreshReaderFallback = { reader.refreshAfterAggregateFailure(autoConnect: false) }

        let index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)
        let infoIndex = await waitForPath("api/reader/info", after: index + 1)
        let healthIndex = await waitForPath("api/health", after: index + 1)
        api.complete(infoIndex, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        let statusIndex = await waitForPath("api/reader/vowifi", after: index + 1)
        let newer = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-c").utf8))
        reader.applyInfo(newer)
        api.complete(statusIndex, json: statusJSON(smsReady: true, simID: "synthetic-sim-b"))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.usimInfo?.iccid == "synthetic-sim-c" && reader.voWiFiStatus == nil,
               "fallback VoWiFi reply cannot restore a profile changed during status read")

        api.complete(healthIndex, json: #"{"ok":true}"#)
        let moduleStatusIndex = await waitForPath("api/status", after: healthIndex + 1)
        api.complete(moduleStatusIndex, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
    }

    @MainActor static func fallbackRejectsUnseenProfileChangeBetweenInfoAndStatus() async {
        let reader = readyStore()
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.refreshReaderFallback = { reader.refreshAfterAggregateFailure(autoConnect: false) }

        let index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)
        let infoIndex = await waitForPath("api/reader/info", after: index + 1)
        let healthIndex = await waitForPath("api/health", after: index + 1)
        api.complete(infoIndex, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        let statusIndex = await waitForPath("api/reader/vowifi", after: index + 1)
        expect(reader.usimInfo?.iccid == "synthetic-sim-b", "fallback cached profile B identity")

        // The card switched physically to C after the info response, before
        // the Go status response. No Swift profile-change callback occurred.
        api.complete(statusIndex, json: statusJSON(smsReady: true, simID: "synthetic-sim-c"))
        await waitUntil { !reader.listRefreshInFlight }
        expect(reader.voWiFiStatus == nil,
               "ready status for unseen profile C cannot display under profile B identity")

        api.complete(healthIndex, json: #"{"ok":true}"#)
        let moduleStatusIndex = await waitForPath("api/status", after: healthIndex + 1)
        api.complete(moduleStatusIndex, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
    }

    @MainActor static func failedAggregateFallsBackToDirectStatus() async {
        let reader = readyStore()
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        gateway.readerGeneration = { reader.generation }
        gateway.applyReaderSnapshot = { snapshot, issues, generation, instanceID in
            reader.applySnapshot(snapshot, issues: issues,
                                 expectedGeneration: generation, autoConnect: false,
                                 instanceID: instanceID)
        }
        var fallbackInvocations = 0
        gateway.refreshReaderFallback = {
            fallbackInvocations += 1
            reader.refreshAfterAggregateFailure(autoConnect: false)
        }

        let index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)

        // Module health remains suspended while the independent reader path
        // verifies card identity before reading IMS/SMS readiness.
        let infoIndex = await waitForPath("api/reader/info", after: index + 1)
        expect(reader.voWiFiStatus == nil, "aggregate failure withdrew old readiness immediately")
        api.complete(infoIndex, json: readerInfoJSON(
            readerName: "Fixture Reader", simID: "synthetic-sim", imsi: "synthetic-imsi"))
        let readerIndex = await waitForPath("api/reader/vowifi", after: index + 1)
        let healthIndex = await waitForPath("api/health", after: index + 1)
        api.complete(readerIndex, json: statusJSON(smsReady: false, callReady: false))
        await waitUntil { reader.voWiFiStatus?.smsReady == false }
        expect(reader.voWiFiStatus?.callReady == false, "fallback withdrew stale call readiness")
        expect(api.pendingCount == 1, "module health remains pending while reader has recovered its status")

        api.complete(healthIndex, json: #"{"ok":true}"#)
        let moduleStatusIndex = await waitForPath("api/status", after: healthIndex + 1)
        api.complete(moduleStatusIndex, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
        for _ in 0..<20 { await Task.yield() }
        expect(fallbackInvocations == 1,
               "aggregate failure must launch identity-bound reader fallback exactly once")

        // When the aggregate path recovers it becomes authoritative again;
        // the exceptional direct read must not pin the UI in a failed state.
        let recoveryIndex = api.requests.count
        for _ in 0..<1_000 where api.requests.count <= recoveryIndex {
            gateway.refresh()
            await Task.yield()
        }
        await waitForRequest(recoveryIndex, path: "api/v2/device-snapshot")
        let recovered = """
            {"schema_version":2,"instance_id":"fixture-rust","revision":1,
             "ok":true,"complete":true,"captured_at_ms":1,"source":"rust-go-shadow",
             "module":{},"reader":\(readerSnapshotJSON),"issues":[]}
            """
        api.complete(recoveryIndex, json: recovered)
        await waitUntil { reader.voWiFiStatus?.smsReady == true }
    }

    @MainActor static func repeatedDirectFailuresWithdrawOldReadiness() async {
        let reader = readyStore()
        for attempt in 1...2 {
            let index = api.requests.count
            reader.refreshVoWiFi(force: true)
            await waitForRequest(index, path: "api/reader/vowifi")
            api.fail(index)
            await waitUntil { !reader.voWiFiRefreshInFlight }
            if attempt == 1 {
                expect(reader.voWiFiStatus?.smsReady == true, "one brief local read failure is tolerated")
            }
        }
        expect(reader.voWiFiStatus == nil, "two failed status reads clear previous ready state")
    }

    @MainActor static func aggregateFailureDoesNotDisturbReaderAction() async {
        let gateway = DeviceSnapshotStore(backend: BackendProcess())
        var readerFallbacks = 0
        gateway.readerActionInFlight = { true }
        gateway.refreshReaderFallback = { readerFallbacks += 1 }

        var index = api.requests.count
        gateway.refresh()
        await waitForRequest(index, path: "api/v2/device-snapshot")
        api.fail(index)
        index += 1
        await waitForRequest(index, path: "api/health")
        api.complete(index, json: #"{"ok":true}"#)
        index += 1
        await waitForRequest(index, path: "api/status")
        api.complete(index, json: "{}")
        await waitUntil { gateway.state.readPath == "rust-endpoint-fallback" }
        for _ in 0..<20 { await Task.yield() }
        expect(readerFallbacks == 0,
               "aggregate failure cannot start fallback during reader card action")
    }

    @MainActor static func lateDirectReplyCannotRestoreOldCard() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(index, path: "api/reader/vowifi")
        reader.clearRuntimeState()
        api.complete(index, json: statusJSON(smsReady: true))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil && !reader.connected,
               "late status from prior generation cannot revive disconnected card")
    }

    @MainActor static func lateDirectReplyCannotRestoreOldProfile() async {
        let reader = readyStore()
        let oldIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(oldIndex, path: "api/reader/vowifi")

        let newInfo = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-b").utf8))
        reader.applyInfo(newInfo)
        expect(reader.voWiFiStatus == nil, "profile switch immediately hides former readiness")
        expect(!reader.voWiFiRefreshInFlight, "profile switch releases old direct-read slot")

        let newIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(newIndex, path: "api/reader/vowifi")
        api.complete(oldIndex, json: statusJSON(smsReady: true))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil, "old profile reply cannot restore ready state")
        expect(reader.voWiFiRefreshInFlight, "old profile reply cannot release new read slot")

        api.complete(newIndex, json: statusJSON(
            smsReady: false, callReady: false, simID: "synthetic-sim-b"))
        await waitUntil { reader.voWiFiStatus?.smsReady == false }
        expect(reader.voWiFiStatus?.callReady == false, "new profile status remains authoritative")
    }

    @MainActor static func lateDirectReplyCannotRestoreOldSelectedReader() async {
        let reader = readyStore()
        let oldIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(oldIndex, path: "api/reader/vowifi")

        let changed = """
            {"readers":["Fixture Reader","Fixture Reader B"],"selected":"Fixture Reader B",
             "info":\(readerInfoJSON(readerName: "Fixture Reader B", simID: "synthetic-sim-b")),
             "vowifi":null}
            """
        let snapshot = try! JSONDecoder().decode(ReaderDeviceSnapshot.self, from: Data(changed.utf8))
        reader.applySnapshot(snapshot, issues: [], expectedGeneration: reader.generation, autoConnect: false)
        expect(reader.name == "Fixture Reader B" && reader.voWiFiStatus == nil,
               "selected reader change hides former readiness")
        expect(!reader.voWiFiRefreshInFlight, "selected reader change releases old direct-read slot")

        let newIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(newIndex, path: "api/reader/vowifi")
        api.fail(oldIndex)
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil, "old reader failure cannot publish an obsolete status")
        expect(reader.voWiFiRefreshInFlight, "old reader failure cannot release new read slot")

        api.complete(newIndex, json: statusJSON(
            smsReady: false, callReady: false,
            readerID: "Fixture Reader B", simID: "synthetic-sim-b"))
        await waitUntil { reader.voWiFiStatus?.smsReady == false }
    }

    @MainActor static func lateDirectReplyCannotRestoreOldReaderAfterListChange() async {
        let reader = readyStore()
        let oldIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(oldIndex, path: "api/reader/vowifi")

        api.readerListJSON = #"{"readers":["Fixture Reader","Fixture Reader B"],"selected":"Fixture Reader B"}"#
        reader.refreshList(autoConnect: false, refreshConnectedInfo: false)
        await waitUntil { reader.name == "Fixture Reader B" }
        api.readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"#
        expect(reader.voWiFiStatus == nil, "reader list change hides former readiness")
        expect(!reader.voWiFiRefreshInFlight, "reader list change releases old direct-read slot")

        let infoIndex = await waitForPath("api/reader/info", after: oldIndex + 1)
        api.complete(infoIndex, json: readerInfoJSON(readerName: "Fixture Reader B", simID: "synthetic-sim-b"))
        await waitUntil { reader.usimInfo?.iccid == "synthetic-sim-b" }

        let newIndex = api.requests.count
        reader.refreshVoWiFi(force: true)
        await waitForRequest(newIndex, path: "api/reader/vowifi")
        api.complete(oldIndex, json: statusJSON(smsReady: true))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.voWiFiStatus == nil, "old list-selected reader reply cannot restore ready state")
        expect(reader.voWiFiRefreshInFlight, "old reply cannot release new list-selected reader read slot")

        api.complete(newIndex, json: statusJSON(
            smsReady: false, callReady: false,
            readerID: "Fixture Reader B", simID: "synthetic-sim-b"))
        await waitUntil { reader.voWiFiStatus?.smsReady == false }
    }

    @MainActor static func switchSyncStatusCannotRestoreNewerProfile() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.synchronizeAfterProfileSwitch()
        await waitForRequest(index, path: "api/reader/refresh")
        api.complete(index, json: readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        let statusIndex = await waitForPath("api/reader/vowifi", after: index + 1)
        expect(reader.usimInfo?.iccid == "synthetic-sim-b", "switch sync refreshed first profile")

        let newer = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-c").utf8))
        reader.applyInfo(newer)
        api.complete(statusIndex, json: statusJSON(smsReady: true))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.usimInfo?.iccid == "synthetic-sim-c" && reader.voWiFiStatus == nil,
               "late switch-sync status cannot restore prior profile readiness")
    }

    @MainActor static func switchSyncRefreshCannotReapplyOldProfile() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.synchronizeAfterProfileSwitch()
        await waitForRequest(index, path: "api/reader/refresh")

        let newer = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-c").utf8))
        reader.applyInfo(newer)
        api.complete(index, json: readerInfoJSON(readerName: "Fixture Reader", simID: "synthetic-sim-b"))
        for _ in 0..<30 { await Task.yield() }
        expect(reader.usimInfo?.iccid == "synthetic-sim-c" && reader.voWiFiStatus == nil,
               "late switch-sync identity refresh cannot reapply prior profile")
        expect(api.requests.count == index + 1,
               "obsolete switch-sync identity refresh cannot launch a status read")
    }

    @MainActor static func connectingSameSIMReaderWithdrawsOldReadiness() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.connect("Fixture Reader B")
        await waitForRequest(index, path: "api/reader/connect")
        expect(reader.voWiFiStatus == nil,
               "connect start must withdraw old reader readiness before backend teardown")
        api.complete(index, json: readerInfoJSON(
            readerName: "Fixture Reader B", simID: "synthetic-sim", imsi: "synthetic-imsi"))
        await waitUntil { reader.name == "Fixture Reader B" && !reader.actionInFlight }
        expect(reader.voWiFiStatus == nil,
               "same SIM moved to a different reader must not retain prior reader session")
    }

    @MainActor static func sameSIMReaderIdentityChangeWithdrawsReadiness() {
        let reader = readyStore()
        let changed = try! JSONDecoder().decode(
            ReaderInfoResponse.self,
            from: Data(readerInfoJSON(
                readerName: "Fixture Reader B", simID: "synthetic-sim", imsi: "synthetic-imsi").utf8))
        reader.applyInfo(changed)
        expect(reader.name == "Fixture Reader B" && reader.voWiFiStatus == nil,
               "reader identity change must clear readiness even with unchanged SIM identifiers")
    }

    @MainActor static func disconnectingReaderWithdrawsOldReadiness() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.disconnect()
        await waitForRequest(index, path: "api/reader/disconnect")
        expect(reader.voWiFiStatus == nil,
               "disconnect start must withdraw old readiness before backend teardown")
        api.complete(index, json: "{}")
        await waitUntil { !reader.connected && !reader.actionInFlight }
        expect(reader.voWiFiStatus == nil, "disconnected reader cannot remain ready")
    }

    @MainActor static func failedReaderTransitionsRecheckStatus() async {
        for disconnecting in [false, true] {
            let reader = readyStore()
            let index = api.requests.count
            if disconnecting { reader.disconnect() } else { reader.connect("Fixture Reader B") }
            await waitForRequest(index, path: disconnecting ? "api/reader/disconnect" : "api/reader/connect")
            expect(reader.voWiFiStatus == nil, "operation in flight withdraws unverified readiness")
            api.fail(index)
            let statusIndex = await waitForPath("api/reader/vowifi", after: index + 1)
            api.complete(statusIndex, json: statusJSON(smsReady: true))
            await waitUntil { reader.voWiFiStatus?.smsReady == true }
            expect(reader.connected && !reader.actionInFlight,
                   "failed reader transition rechecks the unchanged backend session")
        }
    }

    @MainActor static func readerMutationsDoNotInterruptActiveVoWiFiAction() async {
        let reader = readyStore()
        let index = api.requests.count
        reader.enableVoWiFi()
        await waitForRequest(index, path: "api/reader/vowifi")
        reader.connect("Fixture Reader B")
        reader.disconnect()
        expect(api.requests.count == index + 1 && reader.voWiFiActionInFlight,
               "reader mutations cannot invalidate an active VoWiFi start operation")
        api.fail(index)
        await waitForRequest(index + 1, path: "api/reader/vowifi?observe_operation=true")
        expect(reader.voWiFiActionInFlight, "transport failure retains the durable fence")
        api.complete(index + 1, json: withOperation(#"{"available":true,"tunnel_state":"disconnected"}"#,
            operationJSON(terminal: true, phase: "failed", outcome: "failed")))
        await waitForRequest(index + 2, path: "api/reader/vowifi")
        api.complete(index + 2, json: #"{"available":true,"tunnel_state":"disconnected"}"#)
        await waitUntil { !reader.voWiFiActionInFlight }
    }

    @MainActor static func progressStagesFollowActualReadiness() {
        let view = ProgressPolicyFixture()
        expect(view.progressStageStatus(stepNum: 4, step: 5, isError: false,
                                        inFlight: false, imsRegistered: true, smsReady: false) == .done,
               "successful REGISTER remains complete while only SMS is unavailable")
        expect(view.progressStageStatus(stepNum: 5, step: 5, isError: false,
                                        inFlight: false, imsRegistered: true, smsReady: false) == .pending,
               "old step five must not paint an unavailable SMS stage green")
        expect(view.progressStageStatus(stepNum: 4, step: 5, isError: false,
                                        inFlight: false, imsRegistered: false, smsReady: false) == .pending,
               "withdrawn IMS registration must not remain complete")
        expect(view.progressStageStatus(stepNum: 5, step: 5, isError: false,
                                        inFlight: false, imsRegistered: false, smsReady: true) == .pending,
               "stale SMS flag cannot complete a stage after IMS registration is withdrawn")
        expect(view.progressStageStatus(stepNum: 5, step: 5, isError: false,
                                        inFlight: false, imsRegistered: true, smsReady: true) == .done,
               "real SMS readiness completes stage five")
        expect(view.progressStageStatus(stepNum: 4, step: 4, isError: false,
                                        inFlight: true, imsRegistered: false, smsReady: false) == .current,
               "IMS registration in progress remains amber")
        expect(view.progressStageStatus(stepNum: 5, step: 5, isError: false,
                                        inFlight: true, imsRegistered: true, smsReady: false) == .current,
               "SMS setup in progress remains amber")
        expect(view.progressStageStatus(stepNum: 5, step: 5, isError: true,
                                        inFlight: false, imsRegistered: false, smsReady: false) == .failed,
               "fatal failure remains red")
    }
}
