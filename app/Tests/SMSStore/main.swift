import Foundation

private struct CheckFailure: Error, CustomStringConvertible { let description: String }
@MainActor private func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CheckFailure(description: message) }
}
@MainActor private func until(_ predicate: () -> Bool) async throws {
    for _ in 0..<2_000 {
        if predicate() { return }
        await Task.yield()
    }
    throw CheckFailure(description: "async test did not reach expected state")
}
@MainActor private func drain() async { for _ in 0..<30 { await Task.yield() } }

@main @MainActor private enum SMSStoreTests {
    static let api = SMSTestAPI.shared
    static let message = "[{\"id\":\"one\",\"sender\":\"888\",\"content\":\"BAL\",\"timestamp\":1}]"
    static func fixture() -> SMSStore {
        api.reset()
        SMSTestDefaults.shared.values = ["smsSeenIDsSeeded": true]
        UNUserNotificationCenter.shared.posts = []
        UNUserNotificationCenter.shared.holdSettings = false
        return SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
    }
    static func refresh(_ store: SMSStore, force: Bool = false) async throws {
        store.refresh(force: force)
        try await until { !store.refreshInFlight }
    }
    static func emptyMailboxAndDeviceTransition() async throws {
        let store = fixture()
        try await refresh(store)
        try await refresh(store)
        try await refresh(store)
        try expect(api.count("POST api/sms/refresh") == 1, "confirmed empty mailbox was repeatedly read from hardware")
        api.defaults["GET api/sms/devices"] = "{\"devices\":[{\"id\":\"dji\",\"name\":\"Module\",\"available\":true,\"ready\":false}]}"
        try await refresh(store)
        try expect(api.count("POST api/sms/refresh") == 2, "newly connected device was not read")
        api.defaults["GET api/sms/devices"] = "{\"devices\":[{\"id\":\"dji\",\"name\":\"Module\",\"available\":true,\"ready\":true}]}"
        try await refresh(store)
        try expect(api.count("POST api/sms/refresh") == 3, "newly ready device was not read")
        try await refresh(store)
        try expect(api.count("POST api/sms/refresh") == 3, "ready device was repeatedly re-read")
        try await refresh(store, force: true)
        try expect(api.count("POST api/sms/refresh") == 4, "manual refresh did not read device")
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        try expect(store.items.count == 1 && store.attentionStore.received == 1,
                   "new message discovery/attention broke after empty mailbox optimization")
        try await refresh(store)
        try expect(store.attentionStore.received == 1 && UNUserNotificationCenter.shared.posts.count == 1,
                   "regular refresh duplicated notification")
    }
    static func forcedRefreshIsCoalesced() async throws {
        let store = fixture()
        try await refresh(store)
        api.heldRoutes.insert("GET api/sms")
        store.refresh()
        try await until { !(api.waiters["GET api/sms"] ?? []).isEmpty }
        store.refresh(force: true)
        store.refresh(force: true)
        try expect(store.busy, "queued manual refresh lost busy feedback")
        api.heldRoutes.remove("GET api/sms")
        api.resolve("GET api/sms", json: "[]")
        try await until { !store.refreshInFlight }
        try expect(api.count("POST api/sms/refresh") == 2, "manual refresh was lost or executed twice")
    }
    static func deletionInvalidatesLateList(senderDeletion: Bool) async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        let item = store.items[0]
        api.heldRoutes.insert("GET api/sms")
        store.refresh()
        try await until { !(api.waiters["GET api/sms"] ?? []).isEmpty }
        api.heldRoutes.remove("GET api/sms")
        api.defaults["GET api/sms"] = "[]"
        if senderDeletion { _ = try await store.deleteMessages(from: " 888 ") }
        else { _ = try await store.deleteMessage(item) }
        await drain()
        api.resolve("GET api/sms", json: message)
        try await until { !store.refreshInFlight }
        await drain()
        try expect(store.items.isEmpty, "late pre-deletion response restored deleted message")
        try expect(api.count("GET api/sms") == 3, "deletion did not schedule reconciliation")
        try expect(store.attentionStore.received == (senderDeletion ? 1 : 0)
                   && UNUserNotificationCenter.shared.posts.count == 1,
                   "stale list repeated attention notification or deletion retained unread")
    }
    static func stoppedLifecycleRejectsOldResultAndError() async throws {
        for reject in [false, true] {
            let store = fixture()
            api.heldRoutes.insert("GET api/sms")
            store.refresh(force: true)
            try await until { !(api.waiters["GET api/sms"] ?? []).isEmpty }
            store.backend.state = .stopped
            store.stopPolling()
            try expect(!store.busy && !store.refreshInFlight, "stop retained busy/in-flight state")
            store.backend.state = .running
            api.heldRoutes.remove("GET api/sms")
            try await refresh(store)
            if reject { api.reject("GET api/sms") }
            else { api.resolve("GET api/sms", json: message) }
            await drain()
            try expect(store.items.isEmpty && store.lastError == nil && store.attentionStore.received == 0,
                       "old lifecycle response/error changed current state")
            try expect(api.count("POST api/sms/refresh") == 2, "backend restart skipped initial device read")
        }
    }
    static func notificationAuthorizationCannotResurrectDeletedItem() async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = message
        let center = UNUserNotificationCenter.shared
        center.holdSettings = true
        store.refresh()
        try await until { center.settingsWaiter != nil }
        try expect(store.items.count == 1, "list waited on notification permission before publishing")
        api.defaults["GET api/sms"] = "[]"
        _ = try await store.deleteMessage(store.items[0])
        try await until { !store.refreshInFlight }
        center.holdSettings = false
        center.settingsWaiter?.resume(returning: UNNotificationSettings())
        center.settingsWaiter = nil
        await drain()
        try expect(store.items.isEmpty && center.posts.isEmpty, "late notification authorization posted deleted message")
    }
    static func oldDeleteCannotMutateNewLifecycle() async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        api.heldRoutes.insert("POST api/sms/delete")
        let item = store.items[0]
        let deletion = Task { try await store.deleteMessage(item) }
        try await until { !(api.waiters["POST api/sms/delete"] ?? []).isEmpty }
        store.stopPolling()
        try await refresh(store)
        api.resolve("POST api/sms/delete", json: "{\"deleted\":true}")
        _ = try await deletion.value
        try expect(store.items.count == 1, "old backend deletion mutated restarted backend snapshot")
    }
    static func newInboundSMSNotifiesLocallyWithoutSwiftForwarding() async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = """
        [{"id":"in-1","sender":"65075","content":"PAC","timestamp":1},
         {"id":"out-1","sender":"65075","content":"sent","timestamp":2,"direction":"out"}]
        """
        try await refresh(store)
        try expect(store.attentionStore.received == 1 && UNUserNotificationCenter.shared.posts.count == 1,
                   "incoming SMS lost its local attention notification")
        try expect(api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "Swift still submits device-SMS forwarding snapshots")
    }
    static func firstLoadStoresRustBaselineWithoutForwardingHistory() async throws {
        api.reset()
        SMSTestDefaults.shared.values = [:]
        UNUserNotificationCenter.shared.posts = []
        api.defaults["GET api/sms"] = """
        [{"id":"old-1","sender":"65075","content":"history-1","timestamp":1},
         {"id":"old-2","sender":"65075","content":"history-2","timestamp":2}]
        """
        let store = SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
        try await refresh(store)
        let ids = Set(store.items.map(\.notificationID))
        let snapshot = store.forwardingAdmissionSnapshot()
        try expect(snapshot.acceptedNotificationIDs.isEmpty && !ids.isEmpty,
                   "current first-load messages mutated the frozen pre-Rust handoff snapshot")
        try expect(snapshot.wasEnabled == nil,
                   "unknown legacy enable state was presented as durable Rust progress")
        try expect(UNUserNotificationCenter.shared.posts.isEmpty,
                   "first inbox load replayed historical notifications")
    }
    static func deviceInboxRefreshDoesNotOwnForwardingProgress() async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        try expect(store.seenIDs.count == 1 && api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "Swift notification polling still owns forwarding admission")
        store.stopPolling()
        let restored = SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
        try await refresh(restored)
        try expect(api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "restarted Swift store resumed forwarding production")
        try expect(UNUserNotificationCenter.shared.posts.count == 1,
                   "restart repeated local SMS notification")
        let state = SMSTestDefaults.shared.values.description
        try expect(!state.contains("BAL") && !state.contains("888"),
                   "notification checkpoint persisted plaintext SMS or peer")
    }
    static func handoffPreservesLegacyForwardingCursor() async throws {
        SMSTestDefaults.shared.values = [
            "smsSeenIDsSeeded": true,
            "smsForwardingAcceptedIDsV1": [String(repeating: "a", count: 64)],
            "smsForwardingWasEnabledV1": false
        ]
        let store = SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
        let snapshot = store.forwardingAdmissionSnapshot()
        try expect(snapshot.acceptedNotificationIDs == [String(repeating: "a", count: 64)] && snapshot.wasEnabled == false,
                   "one-time handoff did not preserve the old Rust-import cursor and disabled state")
    }
    static func samePeerLineRoutingAndVisibility() async throws {
        let store = fixture()
        let json = "[{\"id\":\"a\",\"sender\":\"65075\",\"content\":\"one\",\"timestamp\":1,\"device\":\"reader\"},{\"id\":\"b\",\"sender\":\"65075\",\"content\":\"two\",\"timestamp\":2,\"device\":\"dji\"}]"
        api.defaults["GET api/sms"] = json
        NSApp.isActive = true
        store.attentionStore.viewingThreadID = SMSConversationIdentity.device(lineID: "reader", peer: "65075")
        defer { NSApp.isActive = false }
        try await refresh(store)
        try expect(store.attentionStore.received == 1 && UNUserNotificationCenter.shared.posts.count == 1,
                   "viewing one thread hid an incoming message on another line")
        let info = UNUserNotificationCenter.shared.posts[0].content.userInfo
        try expect(info["device_id"] == "dji" && info["message_id"] == "b", "notification lost exact source")
        try expect(Set(store.items.map(\.threadID)).count == 2, "same peer merged separate lines")
        _ = try await store.deleteConversation([store.items.first { $0.lineID == "reader" }!])
        try expect(api.count("POST api/sms/delete-sender") == 0 && api.count("POST api/sms/delete") == 1,
                   "thread deletion used sender-wide API")
    }
    static func largeMailboxDoesNotReplay() async throws {
        let store = fixture()
        let messages = (0..<2105).map {
            "{\"id\":\"fixture-\($0)\",\"sender\":\"65075\",\"content\":\"body-\($0)\",\"timestamp\":\($0 + 1),\"device\":\"reader\"}"
        }
        api.defaults["GET api/sms"] = "[" + messages.joined(separator: ",") + "]"
        try await refresh(store)
        try await refresh(store)
        try expect(store.seenIDs.count == 2105,
                   "large inbox evicted IDs needed to suppress duplicate local notifications")
        try expect(store.attentionStore.received == 2105 && UNUserNotificationCenter.shared.posts.count == 2105,
                   "large mailbox repeated or skipped local notifications")
        try expect(api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "large inbox still produced device forwarding from Swift")
    }
    static func upgradeMigratesSensitiveLegacySeenIDs() async throws {
        _ = fixture()
        let legacyID = "reader|1.0|65075|legacy secret body"
        SMSTestDefaults.shared.values = ["smsSeenIDsSeeded": true, "smsSeenIDs": [legacyID]]
        let store = SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
        api.defaults["GET api/sms"] = "[{\"id\":\"legacy\",\"sender\":\"65075\",\"content\":\"legacy secret body\",\"timestamp\":1,\"device\":\"reader\"}]"
        try await refresh(store)
        try expect(UNUserNotificationCenter.shared.posts.isEmpty
                   && api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "upgrade replayed previously seen messages")
        try expect(!SMSTestDefaults.shared.values.description.contains("legacy secret body"),
                   "upgrade retained plaintext legacy seen IDs")
    }
    static func localAttentionDoesNotWaitForForwarding() async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        try expect(!store.refreshInFlight && store.attentionStore.received == 1
                   && UNUserNotificationCenter.shared.posts.count == 1,
                   "local attention was delayed by forwarding admission")
        try expect(api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "Swift still waits on Rust forwarding acknowledgement")
    }
    static func handoffSnapshotRemainsFrozenAsInboxChanges() async throws {
        SMSTestDefaults.shared.values = [
            "smsSeenIDsSeeded": true,
            "smsSeenIDs": [String(repeating: "a", count: 64)],
            "smsSeenIDsOpaqueV1": true,
            "smsForwardingAcceptedIDsV1": [String(repeating: "a", count: 64)],
            "smsForwardingWasEnabledV1": true
        ]
        let store = SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
        let initial = store.forwardingAdmissionSnapshot()
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        let afterInbox = store.forwardingAdmissionSnapshot()
        try expect(afterInbox == initial,
                   "new local inbox notifications mutated the immutable Rust handoff cursor")
        try expect(api.count("POST api/sms/forwarding/device-snapshot") == 0,
                   "Swift resumed production while refreshing after handoff snapshot")
    }
    static func subscriptionIsolationAndExplicitReply() async throws {
        let store = fixture()
        let a = "sub1_" + String(repeating: "a", count: 64)
        let b = "sub1_" + String(repeating: "b", count: 64)
        let source = "{\"sender\":\"888\",\"content\":\"same\",\"timestamp\":1,\"device\":\"reader\""
        let decoder = JSONDecoder()
        let legacy = try decoder.decode(SMSItem.self, from: Data((source + "}").utf8))
        let first = try decoder.decode(SMSItem.self, from: Data((source + ",\"subscription_id\":\"" + a + "\"}").utf8))
        let second = try decoder.decode(SMSItem.self, from: Data((source + ",\"subscription_id\":\"" + b + "\"}").utf8))
        try expect(Set([legacy.threadID, first.threadID, second.threadID]).count == 3, "old and replacement SIM conversation merged")
        try expect(Set([legacy.notificationID, first.notificationID, second.notificationID]).count == 3, "notification identities crossed SIMs")
        try expect(legacy.threadID == SMSConversationIdentity.device(lineID: "reader", peer: "888"), "legacy thread ID changed")
        let out1 = try decoder.decode(SMSItem.self, from: Data((source + ",\"direction\":\"out\",\"send_request_id\":\"request-1\"}").utf8))
        let out2 = try decoder.decode(SMSItem.self, from: Data((source + ",\"direction\":\"out\",\"send_request_id\":\"request-2\"}").utf8))
        try expect(out1.id != out2.id && out1.threadID == out2.threadID, "distinct sends in same millisecond collapsed or changed conversation")
        let current = SMSDeviceStatus(id: "reader", name: "replacement", kind: "reader", phone: "12345", available: true, ready: true, detail: nil, subscriptionID: b)
        try expect(SMSDeviceStatus.replySelection(lineID: "reader", subscriptionID: nil, devices: [current]).isEmpty, "legacy history silently selects current SIM")
        try expect(SMSDeviceStatus.replySelection(lineID: "reader", subscriptionID: a, devices: [current]).isEmpty, "old SIM silently selects replacement")
        try expect(SMSDeviceStatus.replySelection(lineID: "reader", subscriptionID: b, devices: [current]) == current.selectionID, "matching SIM cannot reply")
        let history = SMSDeviceStatus.historyLabel(lineID: "reader", subscriptionID: a, devices: [current], masked: false)
        try expect(!history.contains("12345") && history.contains("历史"), "old history relabeled with new number")
        let unknown = SMSDeviceStatus.historyLabel(lineID: "reader", subscriptionID: nil, devices: [current], masked: false)
        try expect(unknown.contains("未知"), "legacy history gained false ownership")
        store.requestOpenSMS(sender: "888", lineID: "reader", messageID: first.notificationID, subscriptionID: a)
        try expect(store.pendingOpenSubscriptionID == a, "notification lost original subscription")
        store.consumeOpenRequest()
        try expect(store.pendingOpenSubscriptionID == nil, "consumed notification retained stale subscription")
    }
    static func physicalDeleteForwardsOpaqueProof() async throws {
        let store = fixture()
        let token = String(repeating: "a", count: 64)
        api.defaults["GET api/sms"] = "[{\"id\":\"physical\",\"sender\":\"888\",\"content\":\"fixture\",\"timestamp\":1,\"module_storage\":\"SM\",\"module_index\":4,\"deletion_token\":\"\(token)\"}]"
        try await refresh(store)
        let item = store.items[0]
        try expect(item.deletionToken == token, "native proof was lost during decoding")
        _ = try await store.deleteMessage(item, refreshAfter: false)
        let data = api.bodies["POST api/sms/delete"]!.first!
        let body = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        try expect(body["deletion_token"] as? String == token && body["index"] as? Int == 4,
                   "physical deletion did not carry its original proof")
        try expect(api.count("POST api/sms/delete") == 1, "physical deletion repeated")
    }

    static func senderDeleteRetainsConcurrentArrival() async throws {
        let store = fixture()
        api.defaults["GET api/sms"] = message
        try await refresh(store)
        api.heldRoutes.insert("POST api/sms/delete-sender")
        let deletion = Task { try await store.deleteMessages(from: "888") }
        try await until { !(api.waiters["POST api/sms/delete-sender"] ?? []).isEmpty }
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .secondsSince1970
        let later = try decoder.decode(SMSItem.self, from: Data("{\"id\":\"later\",\"sender\":\"888\",\"content\":\"new\",\"timestamp\":2}".utf8))
        store.items.append(later)
        api.defaults["GET api/sms"] = "[{\"id\":\"later\",\"sender\":\"888\",\"content\":\"new\",\"timestamp\":2}]"
        api.resolve("POST api/sms/delete-sender", json: "{\"deleted\":true,\"deleted_count\":1}")
        _ = try await deletion.value
        try expect(store.items.contains { $0.id == later.id }, "sender deletion removed a concurrent new item")
        try await until { !store.refreshInFlight }
    }

    static func main() async {
        let tests: [(String, () async throws -> Void)] = [
            ("physical deletion forwards native proof", physicalDeleteForwardsOpaqueProof),
            ("sender deletion preserves concurrent arrival", senderDeleteRetainsConcurrentArrival),
            ("subscription isolation and explicit reply selection", subscriptionIsolationAndExplicitReply),
            ("known empty mailbox, device transitions, manual refresh and new message", emptyMailboxAndDeviceTransition),
            ("in-flight manual refresh is retained and coalesced", forcedRefreshIsCoalesced),
            ("single delete rejects late old list", { try await deletionInvalidatesLateList(senderDeletion: false) }),
            ("sender delete rejects late old list", { try await deletionInvalidatesLateList(senderDeletion: true) }),
            ("stop/restart rejects stale success and error", stoppedLifecycleRejectsOldResultAndError),
            ("notification authorization cannot publish deleted message", notificationAuthorizationCannotResurrectDeletedItem),
            ("incoming SMS notifies locally without Swift forwarding", newInboundSMSNotifiesLocallyWithoutSwiftForwarding),
            ("first SMS load stores a Rust baseline without forwarding history", firstLoadStoresRustBaselineWithoutForwardingHistory),
            ("Swift inbox refresh leaves forwarding progress to Rust", deviceInboxRefreshDoesNotOwnForwardingProgress),
            ("legacy forwarding state survives one-time handoff", handoffPreservesLegacyForwardingCursor),
            ("same-peer lines keep notifications and deletion isolated", samePeerLineRoutingAndVisibility),
            ("large mailbox does not replay after the old 2000-ID cap", largeMailboxDoesNotReplay),
            ("upgrade hashes legacy sensitive seen IDs without replay", upgradeMigratesSensitiveLegacySeenIDs),
            ("local attention does not wait on forwarding", localAttentionDoesNotWaitForForwarding),
            ("forwarding handoff snapshot stays frozen", handoffSnapshotRemainsFrozenAsInboxChanges),
            ("old delete cannot mutate new backend lifecycle", oldDeleteCannotMutateNewLifecycle)
        ]
        let filter = ProcessInfo.processInfo.environment["SMS_STORE_TEST_FILTER"]
        let selectedTests = tests.filter { filter == nil || $0.0.contains(filter!) }
        guard !selectedTests.isEmpty else { print("No SMS store tests matched filter"); exit(2) }
        var failures = 0
        for (name, test) in selectedTests {
            do { try await test(); print("PASS: \(name)") }
            catch { failures += 1; print("FAIL: \(name): \(error)") }
        }
        if failures > 0 { exit(1) }
        print("SMS store: \(selectedTests.count) production async event groups passed (isolated IO; no hardware)")
    }
}
