import Foundation

private final class FaultingESIMCheckpointDefaults: ESIMCheckpointDefaults {
    var values: [String: Any] = [:]
    var synchronizeResult = true

    func object(forKey defaultName: String) -> Any? { values[defaultName] }
    func set(_ value: Any?, forKey defaultName: String) { values[defaultName] = value }
    func removeObject(forKey defaultName: String) { values.removeValue(forKey: defaultName) }
    func synchronize() -> Bool { synchronizeResult }
}

@main
struct ReaderESIMIdentityTests {
    @MainActor
    static func main() async {
        await delayedTerminalRefreshCannotEnterNextMutation()
        await oldOverviewCannotPublishDuringNextMutation()
        await interruptedOperationIsSurfacedUntilRefresh()
        await readyIdentityChange()
        await lateReply(success: true)
        await lateReply(success: false)
        await incompleteIdentityAndReadiness()
        await transientReadinessDuringAction()
        await lateActionCannotAffectNewReader()
        await switchIdentityConfirmation()
        await switchUnknownOutcomeCannotPublishOptimistically()
        await bannerOwnership()
        await disconnectClearsAndReconnects()
        await legacyRecoveryRequiresExplicitReview()
        await refreshIntentSingleRequestAndStructuredOutcomeForBothPrefixes()
        await unknownOperationStateFailsClosedInBothStores()
        await missingObservationRetainsOriginalFenceInBothStores()
        durableMutationCheckpointSurvivesGUIRestart()
        await failedCheckpointPersistenceIsSticky()
        await notificationMutationCheckpointIsDurableAndSticky()
        await corruptMutationCheckpointsFailClosed()
        await terminalReceiptClearsDurableCheckpoint()
        await acceptedDownloadReceiptResumesPollingInBothStores()
        await directDownloadAcceptanceSurvivesStoreRestartInBothStores()
        await viewAppearanceRecoversDurableReaderDownload()
        await overviewCannotReplaceActiveOwnerInBothStores()
        await otherOperationCannotCompleteRecoveredDownloadInBothStores()
        await mismatchedReceiptCanBeRecheckedInBothStores()
        await invalidDownloadReceiptRetainsCheckpointInBothStores()
        expect(ControlledAPI.shared.pendingCount == 0, "all fixture requests completed")
        print("PASS: ReaderESIM identity/recovery groups; profile and notification GUI restart fences; sticky checkpoint persistence failure, durable request IDs, fail-closed operation fences, both refresh prefixes, production Stores + extracted View binding, no hardware/network")
    }

    @MainActor static let api = ControlledAPI.shared
    @MainActor static func refreshIntentSingleRequestAndStructuredOutcomeForBothPrefixes() async {
        let outcome = ESIMRefreshOutcome(operationID: "one-refresh-intent", scope: "reader",
            phase: "failed", outcome: "unknown", attempts: 1, retryPerformed: false,
            message: "卡片读取结果待确认，本次未重试")
        let reader = ReaderESIMStore()
        var index = api.requests.count
        reader.load(forceRefresh: true)
        await waitForRequest(index + 1)
        expect(api.requests[index].path == "api/reader/esim?refresh=true", "reader sends exactly its refresh intent")
        api.fail(index, error: FixtureRefreshError(refreshOutcome: outcome))
        await waitUntil { !reader.isRefreshing }
        expect(api.requests.count == index + 1 && reader.refreshOutcome == outcome,
               "reader renders unknown outcome without a client retry")
        let module = ModuleESIMStore()
        index = api.requests.count
        module.load(refresh: true)
        await waitForRequest(index + 1)
        expect(api.requests[index].path == "api/esim?refresh=true", "module sends exactly its refresh intent")
        api.fail(index, error: FixtureRefreshError(refreshOutcome: outcome))
        await waitUntil { !module.busy }
        expect(api.requests.count == index + 1 && module.refreshOutcome == outcome && module.error == outcome.message,
               "module renders structured failure without resending")
        let pending = ESIMRefreshOutcome(operationID: "retained-pending-refresh", scope: "reader",
            phase: "pending", outcome: "cleanup_pending", attempts: 1, retryPerformed: false,
            message: "卡片读取结果或清理仍待确认，本次意图未重发")
        index = api.requests.count
        reader.load(forceRefresh: true)
        await waitForRequest(index + 1)
        api.fail(index, error: FixtureRefreshError(refreshOutcome: pending))
        await waitUntil { !reader.isRefreshing }
        expect(reader.refreshOutcome == pending && !pending.completed && api.requests.count == index + 1,
               "reader preserves pending operation ID/attempt count without issuing a retry")
        index = api.requests.count
        module.load(refresh: true)
        await waitForRequest(index + 1)
        api.fail(index, error: FixtureRefreshError(refreshOutcome: pending))
        await waitUntil { !module.busy }
        expect(module.refreshOutcome == pending && module.message == nil && module.error == pending.message &&
               api.requests.count == index + 1,
               "module pending receipt never shows green success or causes another request")
        let completed = #"{"operation_id":"backend-intent","scope":"module","phase":"completed","outcome":"succeeded","attempts":2,"retry_performed":true,"message":"服务已完成一次安全重试"}"#
        index = api.requests.count
        module.load(refresh: true)
        await waitForRequest(index + 1)
        let refreshed = String(overview("A").dropLast()) + ",\"refresh\":" + completed + "}"
        api.complete(index, json: refreshed)
        await waitUntil { !module.busy }
        expect(module.refreshOutcome?.completed == true && module.refreshOutcome?.attempts == 2 &&
               module.message == "服务已完成一次安全重试" && api.requests.count == index + 1,
               "Swift displays Rust's one-intent/two-attempt result without making a second request")
        let failed = #"{"operation_id":"backend-failed","scope":"module","phase":"failed","outcome":"unknown","attempts":1,"retry_performed":false,"message":"卡片读取结果待确认，本次未重试"}"#
        index = api.requests.count
        module.load(refresh: true)
        expect(module.message == nil, "a new refresh clears the previous green success immediately")
        await waitForRequest(index + 1)
        let cachedFailure = String(overview("A").dropLast()) + ",\"refresh\":" + failed + "}"
        api.complete(index, json: cachedFailure)
        await waitUntil { !module.busy }
        expect(module.refreshOutcome?.completed == false && module.message == nil &&
               module.error == "卡片读取结果待确认，本次未重试" && api.requests.count == index + 1,
               "HTTP 200 cached fallback renders the Rust failed receipt as an error, never green success")
    }
    @MainActor static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }

    @MainActor static func durableMutationCheckpointSurvivesGUIRestart() {
        let suite = "switch.esim.checkpoint.fixture.\(UUID().uuidString.lowercased())"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        let first = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
        let created = first.begin(scope: "reader", action: "delete")!
        let restarted = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
        expect(restarted.load(scope: "reader") == .valid(created),
               "GUI restart must recover the exact original client request ID")
        expect(restarted.begin(scope: "reader", action: "rename") == created,
               "a pending checkpoint must not silently mint a successor ID")
        expect(restarted.clear(created) && restarted.load(scope: "reader") == .missing,
               "only the exact checkpoint can release the GUI fence")
        defaults.removePersistentDomain(forName: suite)
    }

    @MainActor static func failedCheckpointPersistenceIsSticky() async {
        for scope in ["reader", "module"] {
            let defaults = FaultingESIMCheckpointDefaults()
            defaults.synchronizeResult = false
            let checkpoints = ESIMMutationCheckpointStore(
                defaults: defaults, keyPrefix: "fixture-persistence")
            let count = api.requests.count
            expect(checkpoints.begin(scope: scope, action: "delete") == nil,
                   "failed synchronize must not return a submit-capable checkpoint")
            guard case .corrupt(let diagnostic) = checkpoints.load(scope: scope) else {
                fatalError("failed checkpoint persistence must become a sticky scope fence")
            }
            expect(diagnostic.contains("synchronize/readback failed"),
                   "persistence fence lacks a diagnostic")
            defaults.synchronizeResult = true
            expect(checkpoints.begin(scope: scope, action: "delete") == nil,
                   "later synchronize success bypassed the sticky persistence failure")
            expect(!checkpoints.clear(ESIMMutationCheckpoint(
                scope: scope, action: "delete", clientRequestID: "replacement")),
                "clear/reset bypassed the sticky persistence failure")
            if scope == "reader" {
                let store = ReaderESIMStore(mutationCheckpoints: checkpoints)
                store.clearForReaderDisconnect()
                store.load(forceRefresh: true)
                store.switchProfile(targetProfile())
                store.deleteProfile(targetProfile())
                store.renameProfile(targetProfile(), newName: "blocked")
                expect(store.isBusy && store.banner?.message.contains("损坏") == true,
                       "reader reset did not retain persistence fence")
            } else {
                let store = ModuleESIMStore(mutationCheckpoints: checkpoints)
                store.clear()
                store.load(refresh: true)
                store.switchProfile(targetProfile())
                store.deleteProfile(targetProfile())
                expect(store.busy && store.error?.contains("损坏") == true,
                       "module reset did not retain persistence fence")
            }
            await settle()
            expect(api.requests.count == count,
                   "persistence failure was bypassed by begin/clear/reset and uploaded an eSIM request")
        }
    }

    @MainActor static func notificationMutationCheckpointIsDurableAndSticky() async {
        let item = ESIMNotificationItem(
            sequenceNumber: 23, event: "install", iccid: "8988",
            address: "https://example.invalid", aidHex: "A0000005591010FFFFFFFF8900000100",
            canRetry: true)
        let suite = "switch.notification.checkpoint.fixture.\(UUID().uuidString.lowercased())"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }
        let key = "notification-fixture"
        let firstCheckpoints = ESIMNotificationMutationCheckpointStore(
            defaults: defaults, key: key)
        let firstStore = ReaderESIMStore(notificationMutationCheckpoints: firstCheckpoints)
        var index = api.requests.count
        firstStore.retryNotification(item)
        await waitForRequest(index + 1)
        expect(api.requests[index].path == "api/reader/esim/notifications/retry",
               "notification retry used the explicit mutation route")
        let firstBody = try! JSONSerialization.jsonObject(
            with: api.requests[index].body ?? Data()) as! [String: Any]
        let requestID = firstBody["client_request_id"] as? String
        expect(requestID?.count == 36 && firstBody["sequence_number"] as? Int == 23,
               "notification mutation omitted its durable UUID or sequence")
        api.fail(index, error: FixtureError.outcomeUnknown)
        await waitUntil { firstStore.retryingNotificationID == nil }

        let restartedCheckpoints = ESIMNotificationMutationCheckpointStore(
            defaults: UserDefaults(suiteName: suite)!, key: key)
        let restartedStore = ReaderESIMStore(notificationMutationCheckpoints: restartedCheckpoints)
        index = api.requests.count
        restartedStore.retryNotification(item)
        await waitForRequest(index + 1)
        let replayBody = try! JSONSerialization.jsonObject(
            with: api.requests[index].body ?? Data()) as! [String: Any]
        expect(replayBody["client_request_id"] as? String == requestID,
               "GUI restart minted a successor notification request ID")
        api.complete(index, json: #"{"message":"confirmed"}"#)
        await waitUntil { restartedStore.retryingNotificationID == nil }
        expect(restartedCheckpoints.load() == .missing,
               "terminal notification receipt did not clear the exact checkpoint")

        let faulting = FaultingESIMCheckpointDefaults()
        faulting.synchronizeResult = false
        let failed = ESIMNotificationMutationCheckpointStore(
            defaults: faulting, key: "notification-fault")
        let count = api.requests.count
        expect(failed.begin(item) == nil,
               "unconfirmed notification checkpoint persistence admitted a POST")
        faulting.synchronizeResult = true
        expect(failed.begin(item) == nil,
               "later synchronize success bypassed sticky notification fence")
        let blockedStore = ReaderESIMStore(notificationMutationCheckpoints: failed)
        blockedStore.clearForReaderDisconnect()
        blockedStore.retryNotification(item)
        await settle()
        expect(api.requests.count == count,
               "notification reset/clear bypassed sticky persistence failure")

        let corrupt = FaultingESIMCheckpointDefaults()
        corrupt.values["notification-corrupt"] = "wrong type"
        let corruptStore = ESIMNotificationMutationCheckpointStore(
            defaults: corrupt, key: "notification-corrupt")
        expect(corruptStore.begin(item) == nil,
               "non-Data notification evidence was overwritten")
    }

    @MainActor static func corruptMutationCheckpointsFailClosed() async {
        for scope in ["reader", "module"] {
            let otherScope = scope == "reader" ? "module" : "reader"
            let fixtures: [Any] = [
                Data("not JSON".utf8), Data(#"{"scope":"reader","action":"switch""#.utf8),
                Data("{\"scope\":\"\(otherScope)\",\"action\":\"switch\",\"clientRequestID\":\"old-id\"}".utf8),
                Data("{\"scope\":\"\(scope)\",\"action\":\"future-action\",\"clientRequestID\":\"old-id\"}".utf8),
                Data("{\"scope\":\"\(scope)\",\"action\":\"switch\",\"clientRequestID\":\" \"}".utf8),
                "wrong defaults type"
            ]
            for original in fixtures {
                let suite = "switch.esim.corrupt.\(UUID().uuidString)"
                let defaults = UserDefaults(suiteName: suite)!
                defer { defaults.removePersistentDomain(forName: suite) }
                let key = "fixture.\(scope)"
                defaults.set(original, forKey: key)
                _ = defaults.synchronize()
                let count = api.requests.count
                for _ in 0..<2 {
                    let checkpoints = ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
                    guard case .corrupt(let diagnostic) = checkpoints.load(scope: scope) else {
                        fatalError("existing invalid checkpoint must be explicitly corrupt")
                    }
                    expect(!diagnostic.isEmpty && checkpoints.begin(scope: scope, action: "switch") == nil,
                           "corrupt scope must not mint an ID")
                    expect(!checkpoints.clear(ESIMMutationCheckpoint(scope: scope, action: "switch", clientRequestID: "old-id")),
                           "corrupt data must not be automatically removed")
                    if scope == "reader" {
                        let store = ReaderESIMStore(mutationCheckpoints: checkpoints)
                        expect(store.isBusy && store.banner?.message.contains("损坏") == true, "reader corruption diagnostic and scope fence")
                        store.clearForReaderDisconnect()
                        store.dismissBanner()
                        store.load(forceRefresh: true)
                        store.switchProfile(targetProfile())
                        store.deleteProfile(targetProfile())
                        store.renameProfile(targetProfile(), newName: "changed")
                        store.confirmDownload(operationID: "unknown", confirm: true)
                        expect(store.isBusy && store.banner?.message.contains("损坏") == true, "reader identity reset cannot release corrupt scope")
                    } else {
                        let store = ModuleESIMStore(mutationCheckpoints: checkpoints)
                        expect(store.busy && store.error?.contains("损坏") == true, "module corruption diagnostic and scope fence")
                        store.clear()
                        store.load(refresh: true)
                        store.switchProfile(targetProfile())
                        store.deleteProfile(targetProfile())
                        store.confirmDownload(operationID: "unknown", confirm: true)
                        expect(store.busy && store.error?.contains("损坏") == true, "module reset cannot release corrupt scope")
                    }
                    await settle()
                    expect(api.requests.count == count, "corrupt GUI restart cannot submit requests")
                    expect((defaults.object(forKey: key) as? NSObject) == (original as? NSObject), "corrupt evidence unchanged")
                    expect(checkpoints.load(scope: otherScope) == .missing, "scope fence remains independent")
                }
            }
        }
    }

    @MainActor static func terminalReceiptClearsDurableCheckpoint() async {
        for scope in ["reader", "module"] {
            let suite = "switch.esim.terminal.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let first = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
            let checkpoint = first.begin(scope: scope, action: "delete")!
            let restarted = ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
            let index = api.requests.count
            let receipt = "{\"client_request_id\":\"\(checkpoint.clientRequestID)\",\"action\":\"delete\",\"scope\":\"\(scope)\",\"phase\":\"terminal\",\"response_status\":409,\"operation_id\":\"fixture-terminal\",\"dispatch_count\":1}"
            if scope == "reader" {
                let store = ReaderESIMStore(mutationCheckpoints: restarted)
                store.load()
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "GET" && api.requests[index].path.contains(checkpoint.clientRequestID), "reader recovery only queries exact original ID")
                api.complete(index, json: receipt)
                await waitUntil { !store.isBusy }
            } else {
                let store = ModuleESIMStore(mutationCheckpoints: restarted)
                store.load()
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "GET" && api.requests[index].path.contains(checkpoint.clientRequestID), "module recovery only queries exact original ID")
                api.complete(index, json: receipt)
                await waitUntil { !store.busy }
            }
            expect(restarted.load(scope: scope) == .missing, "normal exact terminal receipt durably clears scope fence")
            let nextGUI = ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
            expect(nextGUI.load(scope: scope) == .missing, "terminal clear survives GUI restart")
        }
    }

    @MainActor static func acceptedDownloadReceiptResumesPollingInBothStores() async {
        for scope in ["reader", "module"] {
            let suite = "switch.esim.download-recovery.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let checkpoint = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
                .begin(scope: scope, action: "download")!
            let restarted = ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
            let operationID = "accepted-\(scope)-download"
            let receipt = """
                {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"\(scope)","phase":"terminal","response_status":202,"operation_id":"intent-\(scope)","dispatch_count":1,"response":\(operation(operationID, state: "running"))}
                """
            let index = api.requests.count
            if scope == "reader" {
                let store = ReaderESIMStore(mutationCheckpoints: restarted)
                store.load()
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "GET" && api.requests[index].path.contains(checkpoint.clientRequestID),
                       "reader restart queries only the original download request ID")
                api.complete(index, json: receipt)
                await waitUntil { store.operation?.id == operationID }
                expect(restarted.load(scope: scope) == .valid(checkpoint) && store.isBusy,
                       "reader accepted receipt retains the durable request and active operation fence")
                expect(store.banner?.tone != .success && store.banner?.message.contains("已受理") == true,
                       "reader HTTP 202 must never claim the download completed")
                try? await Task.sleep(nanoseconds: 1_100_000_000)
                await waitForRequest(index + 2)
                expect(api.requests[index + 1].method == "GET" && api.requests[index + 1].path == "api/reader/esim/operation",
                       "reader accepted receipt resumes operation polling without another download POST")
                api.complete(index + 1, json: "{\"operation\":\(operation(operationID, state: "failed"))}")
                await waitUntil { store.operation?.isFailed == true }
                expect(restarted.load(scope: scope) == .missing && store.banner?.tone != .success && api.requests.count == index + 2,
                       "reader later failure is not converted to success or a duplicate mutation")
            } else {
                let store = ModuleESIMStore(mutationCheckpoints: restarted)
                store.load()
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "GET" && api.requests[index].path.contains(checkpoint.clientRequestID),
                       "module restart queries only the original download request ID")
                api.complete(index, json: receipt)
                await waitUntil { store.operation?.id == operationID }
                expect(restarted.load(scope: scope) == .valid(checkpoint) && store.operation?.blocksNewMutation == true,
                       "module accepted receipt retains the durable request and active operation fence")
                expect(store.message == nil, "module HTTP 202 must not render green completion")
                try? await Task.sleep(nanoseconds: 2_100_000_000)
                await waitForRequest(index + 2)
                expect(api.requests[index + 1].method == "GET" && api.requests[index + 1].path == "api/esim/operation",
                       "module accepted receipt resumes operation polling without another download POST")
                api.complete(index + 1, json: "{\"operation\":\(operation(operationID, state: "failed"))}")
                await waitUntil { store.operation?.isFailed == true }
                expect(restarted.load(scope: scope) == .missing && store.message == nil && api.requests.count == index + 2,
                       "module later failure is not converted to success or a duplicate mutation")
            }
        }
    }

    @MainActor static func directDownloadAcceptanceSurvivesStoreRestartInBothStores() async {
        let request = ESIMDownloadRequest(smdp: "test.invalid", matchingID: nil,
            confirmationCode: nil, aid: nil, imei: nil)
        for scope in ["reader", "module"] {
            let suite = "switch.esim.direct-download.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let checkpoints = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
            let operationID = "direct-\(scope)-download"
            var index = api.requests.count
            if scope == "reader" {
                let original = ReaderESIMStore(mutationCheckpoints: checkpoints)
                original.load()
                await waitForRequest(index + 1)
                api.complete(index, json: overview("A"))
                await waitUntil { original.loadPhase == .loaded }
                index = api.requests.count
                original.startDownload(request)
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "POST" && api.requests[index].path == "api/reader/esim/download",
                       "reader starts one identified download")
                let checkpoint = { () -> ESIMMutationCheckpoint in
                    if case .valid(let value) = checkpoints.load(scope: scope) { return value }
                    fatalError("reader POST did not persist a download request")
                }()
                api.complete(index, json: operation(operationID, state: "running"))
                await waitUntil { original.operation?.id == operationID }
                expect(checkpoints.load(scope: scope) == .valid(checkpoint),
                       "reader direct 202 keeps the durable request until operation terminal")
                original.clearForReaderDisconnect()
                let restarted = ReaderESIMStore(mutationCheckpoints:
                    ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture"))
                index = api.requests.count
                restarted.load()
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "GET" && api.requests[index].path.contains(checkpoint.clientRequestID),
                       "reader restart recovers the original POST request ID")
                let receipt = """
                    {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"reader","phase":"terminal","response_status":202,"operation_id":"intent-reader","dispatch_count":1,"response":\(operation(operationID, state: "running"))}
                    """
                api.complete(index, json: receipt)
                await waitUntil { restarted.operation?.id == operationID }
                restarted.load(recoverPendingMutation: false)
                await waitForRequest(index + 2)
                api.complete(index + 1, json: overview("A"))
                await waitUntil { restarted.banner?.detail?.contains("概览未返回原操作编号") == true }
                expect(restarted.operation?.id == operationID && restarted.isBusy &&
                       checkpoints.load(scope: scope) == .valid(checkpoint),
                       "reader restart retains the original owner when the overview has no operation")
                restarted.load(forceRefresh: true, recoverPendingMutation: false)
                await waitForRequest(index + 3)
                api.complete(index + 2, json: overview("A", operation: operation("other-reader", state: "succeeded")))
                await waitUntil { !restarted.isRefreshing }
                expect(restarted.operation?.id == operationID && restarted.isBusy &&
                       checkpoints.load(scope: scope) == .valid(checkpoint),
                       "reader restart ignores an unrelated overview terminal and retains the original fence")
                try? await Task.sleep(nanoseconds: 1_100_000_000)
                await waitForRequest(index + 4)
                api.complete(index + 3, json: "{\"operation\":\(operation(operationID, state: "failed"))}")
                await waitUntil { restarted.operation?.isFailed == true }
                expect(checkpoints.load(scope: scope) == .missing && restarted.banner?.tone != .success,
                       "reader releases the durable request only after its own terminal result")
            } else {
                let original = ModuleESIMStore(mutationCheckpoints: checkpoints)
                original.load()
                await waitForRequest(index + 1)
                api.complete(index, json: overview("A"))
                await waitUntil { !original.busy }
                index = api.requests.count
                original.download(request)
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "POST" && api.requests[index].path == "api/esim/download",
                       "module starts one identified download")
                let checkpoint = { () -> ESIMMutationCheckpoint in
                    if case .valid(let value) = checkpoints.load(scope: scope) { return value }
                    fatalError("module POST did not persist a download request")
                }()
                api.complete(index, json: operation(operationID, state: "running"))
                await waitUntil { original.operation?.id == operationID }
                expect(checkpoints.load(scope: scope) == .valid(checkpoint),
                       "module direct 202 keeps the durable request until operation terminal")
                original.clear()
                let restarted = ModuleESIMStore(mutationCheckpoints:
                    ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture"))
                index = api.requests.count
                restarted.load()
                await waitForRequest(index + 1)
                expect(api.requests[index].method == "GET" && api.requests[index].path.contains(checkpoint.clientRequestID),
                       "module restart recovers the original POST request ID")
                let receipt = """
                    {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"module","phase":"terminal","response_status":202,"operation_id":"intent-module","dispatch_count":1,"response":\(operation(operationID, state: "running"))}
                    """
                api.complete(index, json: receipt)
                await waitUntil { restarted.operation?.id == operationID }
                try? await Task.sleep(nanoseconds: 2_100_000_000)
                await waitForRequest(index + 2)
                api.complete(index + 1, json: "{\"operation\":\(operation(operationID, state: "failed"))}")
                await waitUntil { restarted.operation?.isFailed == true }
                expect(checkpoints.load(scope: scope) == .missing && restarted.message == nil,
                       "module releases the durable request only after its own terminal result")
            }
        }
    }

    @MainActor static func overviewCannotReplaceActiveOwnerInBothStores() async {
        let original = operation("overview-original-owner", state: "running")
        let other = operation("overview-other-owner", state: "succeeded")
        let reader = ReaderESIMStore()
        var index = api.requests.count
        reader.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: original))
        await waitUntil { reader.operation?.id == "overview-original-owner" }
        index = api.requests.count
        reader.load(recoverPendingMutation: false)
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: other))
        await waitUntil { reader.banner?.detail?.contains("概览未返回原操作编号") == true }
        expect(reader.operation?.id == "overview-original-owner" && reader.isBusy,
               "reader overview cannot replace a blocking owner with another terminal ID")
        reader.clearForReaderDisconnect()

        let module = ModuleESIMStore()
        index = api.requests.count
        module.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: original))
        await waitUntil { module.operation?.id == "overview-original-owner" && !module.busy }
        index = api.requests.count
        module.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: other))
        await waitUntil { module.error?.contains("概览未返回原操作编号") == true }
        expect(module.operation?.id == "overview-original-owner" && module.operation?.blocksNewMutation == true,
               "module overview cannot replace a blocking owner with another terminal ID")
        module.clear()
    }

    @MainActor static func viewAppearanceRecoversDurableReaderDownload() async {
        let suite = "switch.esim.view-restart.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let checkpoints = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
        let checkpoint = checkpoints.begin(scope: "reader", action: "download")!
        let restarted = ESIMMutationCheckpointStore(
            defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
        let view = ReaderCardViewBindingFixture(mutationCheckpoints: restarted)
        DashboardStore.shared = view.store
        expect(view.readerESIMStore.isBusy && restarted.load(scope: "reader") == .valid(checkpoint),
               "reader Store restart begins fenced before the View appears")
        let index = api.requests.count
        // This is the production View's onAppear binding; do not call Store.load().
        view.deliverContext(name: "USB")
        await waitForRequest(index + 1)
        expect(api.requests[index].method == "GET" &&
               api.requests[index].path.contains("client_request_id=\(checkpoint.clientRequestID)"),
               "View appearance recovers the exact persisted reader request")
        view.deliverContext(name: "USB")
        await settle()
        expect(api.requests.count == index + 1,
               "same-context updates cannot send concurrent receipt queries")
        api.fail(index)
        await waitUntil { view.readerESIMStore.banner?.detail?.contains("无法读取原 Rust 回执") == true }
        expect(view.readerESIMStore.isBusy && restarted.load(scope: "reader") == .valid(checkpoint),
               "failed receipt read keeps the same durable request fenced")
        // A later onAppear with unchanged reader context must retry only the GET.
        view.deliverContext(name: "USB")
        await waitForRequest(index + 2)
        expect(api.requests[index + 1].method == "GET" &&
               api.requests[index + 1].path.contains("client_request_id=\(checkpoint.clientRequestID)"),
               "same-context View reappearance rechecks the original receipt without a POST")
        let operationID = "view-recovered-download"
        api.complete(index + 1, json: """
            {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"reader","phase":"terminal","response_status":202,"operation_id":"intent-view","dispatch_count":1,"response":\(operation(operationID, state: "running"))}
            """)
        await waitUntil { view.readerESIMStore.operation?.id == operationID }
        expect(view.readerESIMStore.isBusy && restarted.load(scope: "reader") == .valid(checkpoint) &&
               view.readerESIMStore.banner?.tone != .success,
               "View-triggered 202 recovery retains the durable write fence without claiming completion")
        view.deliverContext(name: "USB")
        await settle()
        expect(api.requests.count == index + 2,
               "same-context updates do not restart an already tracked download")
        try? await Task.sleep(nanoseconds: 1_100_000_000)
        await waitForRequest(index + 3)
        api.complete(index + 2, json: "{\"operation\":\(operation(operationID, state: "failed"))}")
        await waitUntil { view.readerESIMStore.operation?.isFailed == true }
        expect(restarted.load(scope: "reader") == .missing && api.requests.count == index + 3,
               "only the same operation terminal clears the View-recovered checkpoint")
    }

    @MainActor static func invalidDownloadReceiptRetainsCheckpointInBothStores() async {
        for scope in ["reader", "module"] {
            let invalidResponses = [
                "",
                ",\"response\":{}",
                ",\"response\":\(operation("wrong-kind", state: "running").replacingOccurrences(of: "download_profile", with: "switch_profile"))",
                ",\"response\":\(operation("unknown-state", state: "paused_by_backend"))"
            ]
            for response in invalidResponses {
                let suite = "switch.esim.invalid-download.\(UUID().uuidString)"
                let defaults = UserDefaults(suiteName: suite)!
                defer { defaults.removePersistentDomain(forName: suite) }
                let checkpoint = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
                    .begin(scope: scope, action: "download")!
                let restarted = ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
                let receipt = """
                    {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"\(scope)","phase":"terminal","response_status":202,"operation_id":"intent-invalid","dispatch_count":1\(response)}
                    """
                let index = api.requests.count
                if scope == "reader" {
                    let store = ReaderESIMStore(mutationCheckpoints: restarted)
                    store.load()
                    await waitForRequest(index + 1)
                    api.complete(index, json: receipt)
                    await waitUntil { store.banner?.detail?.contains("缺少有效下载操作") == true }
                    expect(store.isBusy && store.operation == nil && store.banner?.tone == .warning,
                           "invalid reader 202 response keeps a visible non-success fence")
                } else {
                    let store = ModuleESIMStore(mutationCheckpoints: restarted)
                    store.load()
                    await waitForRequest(index + 1)
                    api.complete(index, json: receipt)
                    await waitUntil { store.error?.contains("缺少有效下载操作") == true }
                    expect(store.busy && store.operation == nil && store.message == nil,
                           "invalid module 202 response keeps a visible non-success fence")
                }
                expect(restarted.load(scope: scope) == .valid(checkpoint) && api.requests.count == index + 1,
                       "invalid 202 response preserves the exact durable request and never resubmits")
            }
        }
    }

    @MainActor static func otherOperationCannotCompleteRecoveredDownloadInBothStores() async {
        for scope in ["reader", "module"] {
            let suite = "switch.esim.other-operation.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let checkpoint = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
                .begin(scope: scope, action: "download")!
            let restarted = ESIMMutationCheckpointStore(defaults: UserDefaults(suiteName: suite)!, keyPrefix: "fixture")
            let originalID = "original-\(scope)-download"
            let receipt = """
                {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"\(scope)","phase":"terminal","response_status":202,"operation_id":"intent-\(scope)","dispatch_count":1,"response":\(operation(originalID, state: "running"))}
                """
            let index = api.requests.count
            if scope == "reader" {
                let store = ReaderESIMStore(mutationCheckpoints: restarted)
                store.load()
                await waitForRequest(index + 1)
                api.complete(index, json: receipt)
                await waitUntil { store.operation?.id == originalID }
                try? await Task.sleep(nanoseconds: 1_100_000_000)
                await waitForRequest(index + 2)
                api.complete(index + 1, json: "{\"operation\":\(operation("other-success", state: "succeeded"))}")
                await waitUntil { store.banner?.detail?.contains("其他操作编号") == true }
                expect(store.operation?.id == originalID && store.isBusy && store.banner?.tone == .warning,
                       "reader cannot use a different successful operation to complete the recovered download")
            } else {
                let store = ModuleESIMStore(mutationCheckpoints: restarted)
                store.load()
                await waitForRequest(index + 1)
                api.complete(index, json: receipt)
                await waitUntil { store.operation?.id == originalID }
                try? await Task.sleep(nanoseconds: 2_100_000_000)
                await waitForRequest(index + 2)
                api.complete(index + 1, json: "{\"operation\":\(operation("other-success", state: "succeeded"))}")
                await waitUntil { store.error?.contains("其他操作编号") == true }
                expect(store.operation?.id == originalID && store.operation?.blocksNewMutation == true && store.message == nil,
                       "module cannot use a different successful operation to complete the recovered download")
            }
            expect(restarted.load(scope: scope) == .valid(checkpoint) && api.requests.count == index + 2,
                   "different operation leaves the original durable owner fenced without a duplicate POST")
        }
    }

    @MainActor static func mismatchedReceiptCanBeRecheckedInBothStores() async {
        for scope in ["reader", "module"] {
            let suite = "switch.esim.receipt-mismatch.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let checkpoints = ESIMMutationCheckpointStore(defaults: defaults, keyPrefix: "fixture")
            let checkpoint = checkpoints.begin(scope: scope, action: "download")!
            let wrong = """
                {"client_request_id":"different-request","action":"download","scope":"\(scope)","phase":"terminal","response_status":409,"operation_id":"wrong","dispatch_count":1}
                """
            let correct = """
                {"client_request_id":"\(checkpoint.clientRequestID)","action":"download","scope":"\(scope)","phase":"terminal","response_status":409,"operation_id":"original","dispatch_count":1}
                """
            let index = api.requests.count
            if scope == "reader" {
                let store = ReaderESIMStore(mutationCheckpoints: checkpoints)
                store.load()
                await waitForRequest(index + 1)
                api.complete(index, json: wrong)
                await waitUntil { store.banner?.detail?.contains("回执与原请求不一致") == true }
                expect(checkpoints.load(scope: scope) == .valid(checkpoint) && store.isBusy,
                       "reader mismatched receipt retains the exact checkpoint")
                store.load()
                await waitForRequest(index + 2)
                expect(api.requests[index + 1].path.contains(checkpoint.clientRequestID),
                       "reader can recheck the original request after a mismatched receipt")
                api.complete(index + 1, json: correct)
                await waitUntil { checkpoints.load(scope: scope) == .missing }
            } else {
                let store = ModuleESIMStore(mutationCheckpoints: checkpoints)
                store.load()
                await waitForRequest(index + 1)
                api.complete(index, json: wrong)
                await waitUntil { store.error?.contains("回执与原请求不一致") == true }
                expect(checkpoints.load(scope: scope) == .valid(checkpoint) && store.busy,
                       "module mismatched receipt retains the exact checkpoint")
                store.load()
                await waitForRequest(index + 2)
                expect(api.requests[index + 1].path.contains(checkpoint.clientRequestID),
                       "module can recheck the original request after a mismatched receipt")
                api.complete(index + 1, json: correct)
                await waitUntil { checkpoints.load(scope: scope) == .missing }
            }
            expect(api.requests.count == index + 2,
                   "mismatched receipt recovery performs only GETs and never duplicates a download")
        }
    }
    @MainActor static func settle() async {
        for _ in 0..<12 { await Task.yield() }
    }
    @MainActor static func waitForRequest(_ count: Int) async {
        for _ in 0..<1000 {
            if api.requests.count == count { return }
            await Task.yield()
        }
        fatalError("expected \(count) requests, found \(api.requests.count)")
    }
    @MainActor static func waitUntil(_ condition: () -> Bool) async {
        for _ in 0..<1000 {
            if condition() { return }
            await Task.yield()
        }
        fatalError("store did not reach expected state")
    }
    static func overview(
        _ provider: String,
        iccid: String = "synthetic-iccid",
        operation: String? = nil
    ) -> String {
        // Identical EID/ICCID across transports: reader identity, not card
        // contents or readiness booleans, must trigger the reload.
        let operationField = operation.map { ",\"operation\":\($0)" } ?? ""
        return """
        {"card_type":"euicc","profiles":[{"eid":"synthetic-eid","aid_hex":"A000", "profiles":[{"iccid":"\(iccid)","service_provider_name":"\(provider)","state":1}]}],"capabilities":{"can_refresh":true,"can_download":true,"can_switch":true,"can_rename":true,"can_delete":true,"can_probe_phonebook":false,"vendor_serial_available":false}\(operationField)}
        """
    }
    static func operation(_ id: String, state: String) -> String {
        """
        {"id":"\(id)","kind":"download_profile","state":"\(state)","progress":42,"started_at":"2026-10-22T00:00:00Z","updated_at":"2026-10-22T00:00:01Z"}
        """
    }

    @MainActor static func unknownOperationStateFailsClosedInBothStores() async {
        let unknown = operation("future-state-owner", state: "paused_by_backend")
        let view = fixture()
        var index = api.requests.count
        view.deliverContext(name: "USB")
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: unknown))
        await waitUntil { view.readerESIMStore.loadPhase == .loaded }
        expect(view.readerESIMStore.operation?.hasUnknownOutcome == true && view.readerESIMStore.isBusy,
               "a future reader operation state must retain the card-write fence")
        expect(view.readerESIMStore.banner?.tone == .warning,
               "a future reader operation state must never be rendered as green success")
        index = api.requests.count
        view.readerESIMStore.switchProfile(targetProfile())
        await settle()
        expect(api.requests.count == index, "reader mutation is rejected while the Rust outcome is unknown")
        view.readerESIMStore.clearForReaderDisconnect()

        let module = ModuleESIMStore()
        index = api.requests.count
        module.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: unknown))
        await waitUntil { !module.busy }
        expect(module.operation?.hasUnknownOutcome == true && module.error?.contains("结果待确认") == true,
               "a future module operation state is visible as unknown and remains fenced")
        index = api.requests.count
        module.switchProfile(targetProfile())
        await settle()
        expect(api.requests.count == index, "module mutation is rejected while the Rust outcome is unknown")
        module.clear()
    }

    @MainActor static func missingObservationRetainsOriginalFenceInBothStores() async {
        let active = operation("retained-reader-owner", state: "running")
        let view = fixture()
        var index = api.requests.count
        view.deliverContext(name: "USB")
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: active))
        await waitUntil { view.readerESIMStore.loadPhase == .loaded }
        try? await Task.sleep(nanoseconds: 1_100_000_000)
        await waitForRequest(index + 2)
        api.complete(index + 1, json: #"{"operation":null}"#)
        await settle()
        expect(view.readerESIMStore.operation?.id == "retained-reader-owner" && view.readerESIMStore.isBusy,
               "a nil reader observation must not clear the original Rust owner or fence")
        expect(view.readerESIMStore.banner?.message == "eSIM 操作结果待确认",
               "a missing reader observation is displayed as pending confirmation")
        view.readerESIMStore.clearForReaderDisconnect()

        let module = ModuleESIMStore()
        let moduleActive = operation("retained-module-owner", state: "running")
        index = api.requests.count
        module.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A", operation: moduleActive))
        await waitUntil { !module.busy }
        try? await Task.sleep(nanoseconds: 2_100_000_000)
        await waitForRequest(index + 2)
        api.complete(index + 1, json: #"{"operation":null}"#)
        await settle()
        expect(module.operation?.id == "retained-module-owner" && module.operation?.blocksNewMutation == true,
               "a nil module observation must not clear the original Rust owner or fence")
        expect(module.error?.contains("结果待确认") == true,
               "a missing module observation is displayed as pending confirmation")
        module.clear()
    }
    @MainActor static func fixture() -> ReaderCardViewBindingFixture {
        let view = ReaderCardViewBindingFixture()
        DashboardStore.shared = view.store
        return view
    }
    @MainActor static func loadReady(_ view: ReaderCardViewBindingFixture, name: String = "USB", provider: String = "A") async {
        let index = api.requests.count
        view.deliverContext(name: name)
        await waitForRequest(index + 1)
        api.complete(index, json: overview(provider))
        await waitUntil { view.readerESIMStore.loadPhase == .loaded }
        await settle()
    }

    @MainActor static func interruptedOperationIsSurfacedUntilRefresh() async {
        let view = fixture()
        let store = view.readerESIMStore
        let index = api.requests.count
        view.deliverContext(name: "USB")
        await waitForRequest(index + 1)
        let interrupted = #"{"id":"download-restore-001","kind":"download_profile","state":"failed","step":"interrupted","message":"上次下载结果未知。请先刷新 Profile 列表确认卡片状态，再决定是否重新下载。","progress":100,"error_code":"esim_operation_interrupted","error":"上次下载结果未知。请先刷新 Profile 列表确认卡片状态，再决定是否重新下载。","recoverable":false,"started_at":"2026-10-05T00:00:00Z","updated_at":"2026-10-05T00:01:00Z","finished_at":"2026-10-05T00:01:00Z"}"#
        api.complete(index, json: overview("A", operation: interrupted))
        await waitUntil { store.banner?.detail?.contains("结果未知") == true }
        expect(store.operation?.errorCode == "esim_operation_interrupted" && store.operation?.recoverable == false,
               "interrupted Rust download must be visible as an unrecoverable unknown result")

        let refreshIndex = api.requests.count
        store.refresh()
        await waitForRequest(refreshIndex + 1)
        let reconciled = interrupted
            .replacingOccurrences(of: #""recoverable":false"#, with: #""recoverable":true"#)
            .replacingOccurrences(of: "2026-10-05T00:01:00Z", with: "2026-10-05T00:02:00Z")
            .replacingOccurrences(of: "再决定是否重新下载", with: "已刷新 Profile 列表，请核对当前卡片状态后再操作")
        api.complete(refreshIndex, json: overview("A", operation: reconciled))
        await waitUntil { !store.isRefreshing }
        expect(store.operation?.recoverable == true,
               "a successful explicit profile refresh should reconcile the visible operation state")
        expect(store.banner?.detail?.contains("结果未知") == true,
               "the warning remains visible after refresh so the user can inspect the profile list")
    }

    @MainActor static func legacyRecoveryRequiresExplicitReview() async {
        let store = ReaderESIMStore() // No selected or connected reader.
        let interrupted = #"{"id":"legacy-restore-001","kind":"download_profile","state":"failed","progress":100,"error_code":"esim_operation_interrupted","recoverable":false,"started_at":"2026-10-05T00:00:00Z","updated_at":"2026-10-05T00:01:00.123Z"}"#
        let pending = #"{"operation":\#(interrupted),"manual_ack_available":true}"#
        let first = api.requests.count
        let read = Task { await store.refreshLegacyRecovery() }
        await waitForRequest(first + 1)
        expect(api.requests[first].method == "GET" && api.requests[first].path == "api/esim/operation",
               "readerless recovery must inspect the global operation without writing")
        api.complete(first, json: pending)
        await read.value
        guard let candidate = store.legacyRecovery else { fatalError("readerless legacy recovery was hidden") }
        expect(store.overview == nil && candidate.rawUpdatedAt == "2026-10-05T00:01:00.123Z",
               "global recovery must be visible without a card and retain exact CAS timestamp")
        let suffix = ESIMLegacyRecoveryConfirmation.displayedIDSuffix(candidate.id)
        expect(!ESIMLegacyRecoveryConfirmation.canSubmit(reviewed: false, enteredIDSuffix: suffix, operationID: candidate.id),
               "unchecked review is not submittable")
        expect(!ESIMLegacyRecoveryConfirmation.canSubmit(reviewed: true, enteredIDSuffix: "wrong", operationID: candidate.id),
               "incorrect operation ID suffix is not submittable")
        let unchecked = await store.acknowledgeLegacyRecovery(candidate, reviewedByUser: false, enteredIDSuffix: suffix)
        expect(!unchecked,
               "store must also reject unchecked review")
        let wrongID = await store.acknowledgeLegacyRecovery(candidate, reviewedByUser: true, enteredIDSuffix: "wrong")
        expect(!wrongID,
               "store must also reject wrong operation ID")
        expect(api.requests.count == first + 1, "unchecked, mismatched or cancelled review must not POST")

        let refreshIndex = api.requests.count
        let refresh = Task { await store.refreshLegacyRecovery() }
        await waitForRequest(refreshIndex + 1)
        expect(api.requests[refreshIndex].method == "GET", "automatic/manual refresh must only GET")
        api.complete(refreshIndex, json: pending)
        await refresh.value

        let stalePostIndex = api.requests.count
        let stale = Task { await store.acknowledgeLegacyRecovery(candidate, reviewedByUser: true, enteredIDSuffix: suffix) }
        await waitForRequest(stalePostIndex + 1)
        expect(api.requests[stalePostIndex].method == "POST" &&
               api.requests[stalePostIndex].path == "api/esim/operation/acknowledge",
               "only explicit confirmed review may POST")
        let payload = try! JSONSerialization.jsonObject(with: api.requests[stalePostIndex].body!) as! [String: Any]
        expect(payload["operation_id"] as? String == candidate.id &&
               payload["expected_updated_at"] as? String == candidate.rawUpdatedAt &&
               payload["expected_state"] as? String == "failed" &&
               payload["expected_error_code"] as? String == "esim_operation_interrupted" &&
               payload["legacy_manual_ack"] as? Bool == true,
               "POST must carry exact operation/time CAS and explicit legacy acknowledgement")
        api.fail(stalePostIndex) // Models a server-side 409 stale snapshot.
        await waitForRequest(stalePostIndex + 2)
        expect(api.requests[stalePostIndex + 1].method == "GET",
               "stale response must re-read status, never auto-repeat the POST")
        api.complete(stalePostIndex + 1, json: pending)
        let staleAccepted = await stale.value
        expect(!staleAccepted && store.legacyRecovery != nil,
               "failed acknowledgement must leave unknown operation visible")
        expect(api.requests.count == stalePostIndex + 2,
               "stale response must not start a retry or a card operation")

        let successPostIndex = api.requests.count
        let confirmed = Task { await store.acknowledgeLegacyRecovery(candidate, reviewedByUser: true, enteredIDSuffix: suffix) }
        await waitForRequest(successPostIndex + 1)
        let acknowledged = interrupted.replacingOccurrences(of: #""recoverable":false"#, with: #""recoverable":true"#)
        api.complete(successPostIndex, json: acknowledged)
        await waitForRequest(successPostIndex + 2)
        api.complete(successPostIndex + 1, json: #"{"operation":\#(acknowledged),"manual_ack_available":false}"#)
        let accepted = await confirmed.value
        expect(accepted && store.legacyRecovery == nil && store.legacyRecoveryNotice?.contains("结果仍未知") == true,
               "manual acknowledgement removes only the prompt and retains outcome-unknown wording")
    }

    static func targetProfile() -> ESIMProfileContext {
        ESIMProfileContext(eid: "synthetic-eid", aidHex: "A000", profile: ProfileItem(
            iccid: "8986001234567890123", name: "target", serviceProviderName: "target carrier",
            state: 0, stateText: nil, classText: nil))
    }

    @MainActor static func delayedTerminalRefreshCannotEnterNextMutation() async {
        let view = fixture()
        await loadReady(view)
        let store = view.readerESIMStore
        let index = api.requests.count
        store.switchProfile(targetProfile())
        await waitForRequest(index + 1)
        api.complete(index, json: #"{"switch_accepted":true,"reader_info":{"usim_info":{"iccid":"8986001234567890123"}},"operation":{"id":"switch-A","kind":"switch_profile","state":"succeeded","progress":100,"refresh_after_seconds":1,"started_at":"2026-09-30T00:00:00Z","updated_at":"2026-09-30T00:00:01Z"}}"#)
        await waitUntil { store.operation?.id == "switch-A" && store.action == nil }
        store.startDownload(ESIMDownloadRequest(smdp: "test.invalid", matchingID: nil, confirmationCode: nil, aid: nil, imei: nil))
        await waitForRequest(index + 2)
        // One bounded offline delay crosses the previous switch's timer. No
        // real reader, network service or repeated long-running loops are used.
        try? await Task.sleep(nanoseconds: 1_100_000_000)
        expect(api.requests.count == index + 2,
               "terminal refresh from switch A must not issue an eUICC read during download B")
        expect(store.action == .downloading && store.operation == nil && !store.isRefreshing,
               "new write owns its progress and must retire the previous operation card")
        expect(view.store.profileSynchronizations == 1 && store.overview != nil,
               "cancelled terminal refresh must neither clear identity again nor remove the loaded list")
        api.fail(index + 1)
        await waitUntil { !store.isBusy }
    }

    @MainActor static func oldOverviewCannotPublishDuringNextMutation() async {
        for success in [true, false] {
            let view = fixture()
            await loadReady(view)
            let store = view.readerESIMStore
            let index = api.requests.count
            store.reloadAfterMetadataChange()
            await waitForRequest(index + 1)
            store.switchProfile(targetProfile())
            await waitForRequest(index + 2)
            if success { api.complete(index, json: overview("stale carrier")) }
            else { api.fail(index) }
            await settle()
            expect(store.action == .switching(targetProfile().id) && store.banner == nil && !store.isRefreshing,
                   "older read success/failure must not alter the new write's status or notice")
            expect(store.profileGroups[0].profiles[0].profile.serviceProviderName == "A" &&
                   view.store.publishedOperators.compactMap { $0 } == ["A"],
                   "older overview cannot publish stale carrier while a card write is running")
            api.fail(index + 1)
            await waitUntil { !store.isBusy }
        }
    }
    @MainActor static func readyIdentityChange() async {
        let view = fixture()
        await loadReady(view)
        let store = view.readerESIMStore
        store.banner = ESIMBanner(tone: .error, message: "old reader error", detail: nil)
        view.readerESIMExpanded = true
        view.readerShowingDownload = true
        view.readerShowingNotifications = true
        let profile = store.profileGroups[0].profiles[0]
        view.readerRenamingProfile = profile
        view.readerDeletingProfile = profile
        view.readerEnablingProfile = profile
        let index = api.requests.count
        view.deliverContext(name: "BLE:fixture")
        expect(store.overview == nil && store.banner == nil, "reader change immediately removes old overview/banner")
        expect(!view.readerESIMExpanded && !view.readerShowingDownload && !view.readerShowingNotifications, "old sheets closed")
        expect(view.readerRenamingProfile == nil && view.readerDeletingProfile == nil && view.readerEnablingProfile == nil, "old action contexts cleared")
        await waitForRequest(index + 1)
        expect(api.requests[index].method == "GET" && api.requests[index].path == "api/reader/esim", "selection only issues a normal read")
        api.complete(index, json: overview("B"))
        await waitUntil { store.loadPhase == .loaded }
        let notice = ESIMBanner(tone: .success, message: "keep operation notice", detail: nil)
        store.banner = notice
        for _ in 0..<100 { view.deliverContext(name: "BLE:fixture") }
        await settle()
        expect(api.requests.count == index + 1, "unchanged polling does not reload")
        expect(store.banner?.id == notice.id && store.profileGroups[0].profiles[0].profile.serviceProviderName == "B", "unchanged polling does not clear")
        expect(view.store.publishedOperators.compactMap { $0 } == ["A", "B"], "new reader owns operator publication")
    }
    @MainActor static func lateReply(success: Bool) async {
        let view = fixture()
        let old = api.requests.count
        view.deliverContext(name: "USB")
        await waitForRequest(old + 1)
        view.deliverContext(name: "BLE:fixture")
        await waitForRequest(old + 2)
        api.complete(old + 1, json: overview("B"))
        await waitUntil { view.readerESIMStore.loadPhase == .loaded }
        let notice = ESIMBanner(tone: .success, message: "new reader", detail: nil)
        view.readerESIMStore.banner = notice
        if success { api.complete(old, json: overview("A")) } else { api.fail(old) }
        await settle()
        expect(view.readerESIMStore.profileGroups[0].profiles[0].profile.serviceProviderName == "B", "late old response cannot overwrite overview")
        expect(view.readerESIMStore.banner?.id == notice.id && view.readerESIMStore.loadPhase == .loaded, "late old failure cannot overwrite new state")
        expect(view.store.publishedOperators.compactMap { $0 } == ["B"], "late old response cannot publish old operator")
    }
    @MainActor static func incompleteIdentityAndReadiness() async {
        let view = fixture()
        let index = api.requests.count
        view.deliverContext(name: nil)
        view.deliverContext(name: "")
        view.deliverContext(name: "  ")
        view.deliverContext(name: "USB", connected: false)
        view.deliverContext(name: "USB", hasESIM: false)
        await settle()
        expect(api.requests.count == index, "incomplete selected identity/readiness does not read")
        view.deliverContext(name: "USB")
        for _ in 0..<10 { view.deliverContext(name: "USB") }
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A"))
        await waitUntil { view.readerESIMStore.loadPhase == .loaded }
        view.deliverContext(name: "USB", hasESIM: false)
        view.deliverContext(name: "USB")
        await settle()
        expect(api.requests.count == index + 1 && view.readerESIMStore.overview != nil, "readiness-only transitions retain loaded identity")
    }
    @MainActor static func transientReadinessDuringAction() async {
        let view = fixture()
        await loadReady(view)
        let store = view.readerESIMStore
        let index = api.requests.count
        view.readerShowingDownload = true
        store.startDownload(ESIMDownloadRequest(smdp: "test.invalid", matchingID: nil, confirmationCode: nil, aid: nil, imei: nil))
        await waitForRequest(index + 1)
        view.deliverContext(name: "USB", hasESIM: false)
        view.deliverContext(name: "USB")
        await settle()
        expect(store.action == .downloading && view.readerShowingDownload, "readiness transition must not cancel active download or close sheet")
        expect(api.requests.count == index + 1, "readiness transition must not send extra read/write")
        api.fail(index)
        await waitUntil { store.action == nil }
        expect(store.banner?.message == "无法开始下载 Profile", "same-reader operation completion retained")
    }
    @MainActor static func bannerOwnership() async {
        let view = fixture()
        await loadReady(view)
        let store = view.readerESIMStore
        var index = api.requests.count
        store.load()
        await waitForRequest(index + 1)
        api.fail(index)
        await waitUntil { store.banner?.tone == .error }
        index += 1
        store.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A"))
        await waitUntil { store.banner == nil }
        index += 1
        store.load()
        await waitForRequest(index + 1)
        api.fail(index)
        await waitUntil { store.banner?.tone == .error }
        let operationNotice = ESIMBanner(tone: .warning, message: "keep operation warning", detail: nil)
        store.banner = operationNotice
        index += 1
        store.load()
        await waitForRequest(index + 1)
        api.complete(index, json: overview("B"))
        await waitUntil { store.profileGroups.first?.profiles.first?.profile.serviceProviderName == "B" }
        expect(store.banner?.id == operationNotice.id, "successful load must not clear newer operation banner")
    }
    @MainActor static func lateActionCannotAffectNewReader() async {
        for success in [true, false] {
            let view = fixture()
            await loadReady(view)
            let store = view.readerESIMStore
            let old = api.requests.count
            let target = ESIMProfileContext(eid: "synthetic-eid", aidHex: "A000", profile: ProfileItem(
                iccid: "8986001234567890123", name: "old target", serviceProviderName: "old carrier",
                state: 0, stateText: nil, classText: nil))
            store.switchProfile(target)
            await waitForRequest(old + 1)
            view.deliverContext(name: "BLE:fixture")
            await waitForRequest(old + 2)
            api.complete(old + 1, json: overview("B"))
            await waitUntil { store.loadPhase == .loaded }
            if success {
                api.complete(old, json: #"{"reader_info":{"usim_info":{"iccid":"8986001234567890123"}}}"#)
            } else { api.fail(old) }
            await settle()
            expect(store.action == nil && store.banner == nil, "late action result cannot replace new reader state")
            expect(view.store.publishedOperators.compactMap { $0 } == ["A", "B"], "late switch cannot publish old target operator")
            expect(view.store.profileSynchronizations == 0 && api.requests.count == old + 2, "late switch cannot synchronize or reload new reader")
        }
    }
    @MainActor static func switchIdentityConfirmation() async {
        let targetICCID = "8986001234567890123"
        for actualICCID in [targetICCID, targetICCID + "F", "8986001234567890124", ""] {
            let view = fixture()
            await loadReady(view)
            let store = view.readerESIMStore
            let index = api.requests.count
            let target = ESIMProfileContext(eid: "synthetic-eid", aidHex: "A000", profile: ProfileItem(
                iccid: targetICCID, name: "target", serviceProviderName: "target carrier",
                state: 0, stateText: nil, classText: nil))
            store.switchProfile(target)
            await waitForRequest(index + 1)
            api.complete(index, json: """
                {"switch_accepted":true,"reader_info":{"usim_info":{"iccid":"\(actualICCID)"}}}
                """)
            await waitForRequest(index + 2)
            let confirmed = actualICCID == targetICCID || actualICCID == targetICCID + "F"
            expect(view.store.publishedOperators.compactMap { $0 } == (confirmed ? ["A", "target carrier"] : ["A"]),
                   "accepted switch must not publish unconfirmed target operator")
            expect(store.banner?.tone == (confirmed ? .success : .warning), "missing operation cannot imply successful activation")
            // The automatic profile refresh must not undo the identity gate:
            // the eUICC may say enabled while its USIM still presents old data.
            api.complete(index + 1, json: overview("target carrier", iccid: targetICCID))
            await waitUntil { !store.isRefreshing }
            expect(view.store.publishedOperators.compactMap { $0 } == (confirmed ? ["A", "target carrier", "target carrier"] : ["A"]),
                   "profile enabled-state cannot override unconfirmed USIM identity")
            if !confirmed {
                view.store.readerUSIMInfo = FixtureUSIMInfo(iccid: targetICCID)
                store.load()
                await waitForRequest(index + 3)
                api.complete(index + 2, json: overview("target carrier", iccid: targetICCID))
                await waitUntil { view.store.publishedOperators.count == 2 }
                expect(view.store.publishedOperators.compactMap { $0 } == ["A", "target carrier"],
                       "verified USIM identity must allow a later profile publication")
            }
        }
    }
    @MainActor static func switchUnknownOutcomeCannotPublishOptimistically() async {
        for unknown in [true, false] {
            let view = fixture()
            await loadReady(view)
            let store = view.readerESIMStore
            let index = api.requests.count
            let targetICCID = "8986001234567890123"
            let target = ESIMProfileContext(eid: "synthetic-eid", aidHex: "A000", profile: ProfileItem(
                iccid: targetICCID, name: "target", serviceProviderName: "target carrier",
                state: 0, stateText: nil, classText: nil))
            store.switchProfile(target)
            await waitForRequest(index + 1)
            api.fail(index, error: unknown ? FixtureError.outcomeUnknown : FixtureError.failed)
            if unknown {
                await waitForRequest(index + 2)
                let payload = try! JSONSerialization.jsonObject(with: api.requests[index].body!) as! [String: Any]
                let requestID = payload["client_request_id"] as! String
                expect(api.requests[index + 1].method == "GET" &&
                       api.requests[index + 1].path.contains("client_request_id=\(requestID)"),
                       "lost response must recover by the exact durable request ID")
                api.complete(index + 1, json: """
                    {"client_request_id":"\(requestID)","action":"switch","scope":"reader","phase":"unknown","operation_id":"ledger-op","dispatch_count":1}
                    """)
                await waitUntil { store.isBusy && store.banner?.message == "切换 Profile 结果待确认" }
                let blocked = api.requests.count
                store.switchProfile(target)
                await settle()
                expect(api.requests.count == blocked, "unknown receipt must retain the GUI mutation fence")

                store.load(forceRefresh: true)
                await waitForRequest(index + 3)
                expect(api.requests[index + 2].path.contains("client_request_id=\(requestID)"),
                       "GUI recovery must reuse the original request ID")
                view.store.readerUSIMInfo = FixtureUSIMInfo(iccid: targetICCID)
                api.complete(index + 2, json: """
                    {"client_request_id":"\(requestID)","action":"switch","scope":"reader","phase":"terminal","operation_id":"ledger-op","dispatch_count":1,"response_status":200}
                    """)
                await waitForRequest(index + 4)
                api.complete(index + 3, json: overview("target carrier", iccid: targetICCID))
                await waitUntil { !store.isRefreshing }
                expect(view.store.publishedOperators.compactMap { $0 } == ["A", "target carrier"],
                       "terminal exact receipt plus actual USIM identity releases the fence")
                expect(view.store.profileSynchronizations == 0 && api.requests.count == index + 4,
                       "recovery queries the ledger and never resubmits a card mutation")
            } else {
                await waitUntil { store.action == nil }
                expect(store.banner?.message == "切换 Profile 失败",
                       "definite failure remains distinct from unknown outcome")
                store.load(forceRefresh: true)
                await waitForRequest(index + 2)
                api.complete(index + 1, json: overview("target carrier", iccid: targetICCID))
                await waitUntil { !store.isRefreshing }
                expect(view.store.publishedOperators.compactMap { $0 } == ["A", "target carrier"],
                       "definite failure does not retain an unknown-outcome fence")
            }
        }
    }
    @MainActor static func disconnectClearsAndReconnects() async {
        let view = fixture()
        await loadReady(view)
        let index = api.requests.count
        view.deliverContext(name: "USB", connected: false)
        expect(view.readerESIMStore.overview == nil && view.readerESIMStore.loadPhase == .idle, "disconnect clears even if name retained")
        await settle()
        expect(api.requests.count == index, "disconnect does not read")
        view.deliverContext(name: "USB")
        await waitForRequest(index + 1)
        api.complete(index, json: overview("A"))
        await waitUntil { view.readerESIMStore.loadPhase == .loaded }
    }
}
