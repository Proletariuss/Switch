import Foundation
import Darwin
import AppKit

private func require(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
}
private func decode<T: Decodable>(_ type: T.Type, _ text: String) -> T {
    try! JSONDecoder().decode(type, from: Data(text.utf8))
}

@MainActor
private final class FakeAPI: MultiStandbyAPI {
    var profileResponse = decode(MultiStandbyProfilesResponse.self, """
    {"profiles":[
      {"profile_id":"a","card_id":"card","display_name":"A","desired_state":"keep_online","sms_ready":true},
      {"profile_id":"b","card_id":"card","display_name":"B","desired_state":"keep_online","sms_ready":false}
    ],"boot_id":"boot-1","revision":1}
    """)
    var inbox: [String: [MultiStandbyMessage]] = [:]
    var changeEvents: [MultiStandbyChange] = []
    var changeRevision: UInt64 = 0
    var changeCursors: [String?] = []
    var sent: [MultiStandbySendRequest] = []
    var deleted: [(String, String)] = []
    var controls: [MultiStandbyControlRequest] = []
    var retries: [MultiStandbyControlRequest] = []
    var preparedControls: [String: String] = [:]
    var prepareControlCalls = 0
    var failControlling = false
    var reportRequests: [(Int64, String)] = []
    var inventoryRequests = 0
    var archiveCount = 0
    var archiveDatesReversed = false
    var archiveReads = 0
    var historyReads = 0
    var summaryReads = 0
    var failSummaryBeforeCount = 0
    var holdLaterSummaryPage = false
    var holdAllSummaryPages = false
    var summaryWaiters: [(MultiStandbyConversationsResponse, CheckedContinuation<MultiStandbyConversationsResponse, Error>)] = []
    var failSending = false
    var queueBeforeSendError = false
    var sendOutcomeState = "queued"
    var failPreparingBefore = false
    var failPreparingAfter = false
    var failAuthorizingAfter = false
    var intentLedger: [String: MultiStandbySendIntent] = [:]
    var intentReads = 0
    var holdIntentQueries = false
    var intentWaiters: [(MultiStandbySendIntent, CheckedContinuation<MultiStandbySendIntent, Error>)] = []
    var heldProfile: String?
    var holdHistoryOnly = false
    var waiters: [CheckedContinuation<MultiStandbyMessagesResponse, Error>] = []
    var holdProfiles = false
    var profileWaiters: [CheckedContinuation<MultiStandbyProfilesResponse, Error>] = []
    var holdDeletes = false
    var deleteWaiters: [(String, String, CheckedContinuation<Void, Error>)] = []
    func cards() async throws -> MultiStandbyCardsResponse {
        inventoryRequests += 1
        return decode(MultiStandbyCardsResponse.self, """
        {"cards":[{"card_id":"card","reader":"Fixture","profile_count":2,"available":true}],"boot_id":"boot-1"}
        """)
    }
    func profiles(cardID: String) async throws -> MultiStandbyProfilesResponse {
        if holdProfiles { return try await withCheckedThrowingContinuation { profileWaiters.append($0) } }
        return profileResponse
    }
    func messages(profileID: String, cursor: String?) async throws -> MultiStandbyMessagesResponse {
        if archiveCount > 0 {
            archiveReads += 1
            let offset = min(Int(cursor ?? "0") ?? 0, archiveCount)
            let end = min(offset + 100, archiveCount)
            let page = (offset..<end).map { value in
                MultiStandbyMessage(messageID: "archive-\(value + 1)", profileID: profileID,
                    direction: "inbound", peer: "123", body: "history", createdAtMS: Int64(archiveDatesReversed ? archiveCount - value : value + 1),
                    state: "received", kind: "message")
            }
            return MultiStandbyMessagesResponse(messages: page,
                nextCursor: MultiStandbyCursor(String(end)), revision: UInt64(archiveCount))
        }
        if heldProfile == profileID && !holdHistoryOnly {
            return try await withCheckedThrowingContinuation { waiters.append($0) }
        }
        return MultiStandbyMessagesResponse(messages: inbox[profileID] ?? [], nextCursor: nil,
            revision: max(10, changeRevision))
    }
    private func archived(profileID: String) -> [MultiStandbyMessage] {
        let messages = archiveCount > 0 ? (1...archiveCount).map { value in
            MultiStandbyMessage(messageID: "archive-\(value)", profileID: profileID,
                direction: "inbound", peer: "123", body: "history", createdAtMS: Int64(archiveDatesReversed ? archiveCount - value + 1 : value), state: "received", kind: "message")
        } : inbox[profileID] ?? []
        return messages.filter { message in !deleted.contains { $0.0 == profileID && $0.1 == message.messageID } }
    }
    func history(profileID: String, peer: String?, cursor: String?) async throws -> MultiStandbyMessagesResponse {
        historyReads += 1
        if heldProfile == profileID {
            return try await withCheckedThrowingContinuation { waiters.append($0) }
        }
        var messages = archived(profileID: profileID).filter { peer == nil || $0.peer == peer }
        if let cursor {
            let fields = cursor.split(separator: "~", maxSplits: 1)
            let date = Int64(fields[0])!
            let messageID = String(fields[1])
            messages = messages.filter { $0.createdAtMS < date || ($0.createdAtMS == date && $0.messageID < messageID) }
        }
        messages.sort { $0.createdAtMS == $1.createdAtMS ? $0.messageID > $1.messageID : $0.createdAtMS > $1.createdAtMS }
        let more = messages.count > 100
        messages = Array(messages.prefix(100))
        let next = more ? messages.last.map { MultiStandbyCursor("\($0.createdAtMS)~\($0.messageID)") } : nil
        return MultiStandbyMessagesResponse(messages: Array(messages.reversed()), nextCursor: next, revision: UInt64(max(10, max(archiveCount, Int(changeRevision)))))
    }
    func conversations(profileID: String?, conversationID: String?, cursor: String?) async throws -> MultiStandbyConversationsResponse {
        summaryReads += 1
        if failSummaryBeforeCount > 0 {
            failSummaryBeforeCount -= 1
            throw URLError(.cannotConnectToHost)
        }
        let profiles = profileResponse.profiles.map(\.profileID).filter { profileID == nil || profileID == $0 }
        let grouped = Dictionary(grouping: profiles.flatMap { archived(profileID: $0) }) {
            SMSConversationIdentity.digest($0.profileID + "\u{0}" + $0.peer)
        }
        let summaries = grouped.compactMap { id, messages -> MultiStandbyConversation? in
            guard conversationID == nil || conversationID == id else { return nil }
            let latest = messages.max { $0.createdAtMS == $1.createdAtMS ? $0.messageID < $1.messageID : $0.createdAtMS < $1.createdAtMS }!
            return MultiStandbyConversation(conversationID: id, profileID: latest.profileID, peer: latest.peer,
                messageCount: messages.count, latestMessage: latest)
        }.sorted { $0.id < $1.id }.filter { cursor == nil || $0.id > cursor! }
        let page = Array(summaries.prefix(100))
        let result = MultiStandbyConversationsResponse(conversations: page,
            nextCursor: summaries.count > 100 ? page.last.map { MultiStandbyCursor($0.id) } : nil,
            revision: UInt64(max(10, max(archiveCount, Int(changeRevision)))))
        if conversationID == nil && (holdAllSummaryPages || (holdLaterSummaryPage && cursor != nil)) {
            return try await withCheckedThrowingContinuation { summaryWaiters.append((result, $0)) }
        }
        return result
    }
    func changes(cursor: String?) async throws -> MultiStandbyChangesResponse {
        changeCursors.append(cursor)
        let revision = UInt64(cursor ?? "0") ?? 0
        let changes = changeEvents.filter { $0.revision > revision }
        let current = max(changeRevision, changeEvents.last?.revision ?? revision)
        return MultiStandbyChangesResponse(changes: changes,
            nextCursor: MultiStandbyCursor(String(changes.last?.revision ?? current)), revision: current)
    }
    private func controlIntentKey(_ action: String, _ cardID: String, _ profileIDs: [String]?, _ all: Bool?) -> String {
        action + ":" + cardID + ":" + ((profileIDs ?? []).sorted().joined(separator: ",")) + ":" + String(all == true)
    }
    func prepareControlIntent(_ request: MultiStandbyControlIntentRequest) async throws -> MultiStandbyPreparedControlIntent {
        prepareControlCalls += 1
        let key = controlIntentKey(request.action, request.cardID, request.profileIDs, request.all)
        let id = preparedControls[key] ?? UUID().uuidString
        preparedControls[key] = id
        return MultiStandbyPreparedControlIntent(clientRequestID: id)
    }
    func acknowledgeControlIntent(_ request: MultiStandbyControlIntentAcknowledgement) async throws -> MultiStandbyControlIntentAcknowledged {
        let key = controlIntentKey(request.action, request.cardID, request.profileIDs, request.all)
        let acknowledged = preparedControls[key] == request.clientRequestID
        if acknowledged { preparedControls.removeValue(forKey: key) }
        return MultiStandbyControlIntentAcknowledged(acknowledged: acknowledged)
    }
    func control(start: Bool, request: MultiStandbyControlRequest) async throws -> MultiStandbyAccepted {
        controls.append(request)
        if failControlling { throw URLError(.networkConnectionLost) }
        return MultiStandbyAccepted(operationID: "op", accepted: true)
    }
    func retry(request: MultiStandbyControlRequest) async throws -> MultiStandbyAccepted {
        retries.append(request)
        if failControlling { throw URLError(.networkConnectionLost) }
        return MultiStandbyAccepted(operationID: "retry", accepted: true)
    }
    func runtimeReport(sinceMS: Int64, cardID: String) async throws -> MultiStandbyRuntimeReport {
        reportRequests.append((sinceMS, cardID))
        return decode(MultiStandbyRuntimeReport.self, """
        {"since_ms":1,"until_ms":2,"coverage_start_ms":1,"retention_days":30,"event_limit":4096,"events":[],"profiles":[]}
        """)
    }
    func operation(id: String) async throws -> MultiStandbyOperation {
        MultiStandbyOperation(operationID: id, state: "completed", detail: "done")
    }
    func send(_ request: MultiStandbySendRequest) async throws -> MultiStandbySent {
        sent.append(request)
        if !failSending || queueBeforeSendError {
            intentLedger[request.clientRequestID] = makeIntent(request, phase: "queued", state: sendOutcomeState)
        }
        if failSending { throw URLError(.networkConnectionLost) }
        return MultiStandbySent(messageID: "sent", state: "queued", revision: 11)
    }
    private func makeIntent(_ request: MultiStandbySendRequest, phase: String, state: String, canSubmit: Bool = false) -> MultiStandbySendIntent {
        MultiStandbySendIntent(profileID: request.profileID, clientRequestID: request.clientRequestID, phone: request.phone,
            body: request.body, phase: phase, state: state, createdAtMS: 1, messageID: phase == "queued" ? "sent" : nil,
            revision: phase == "queued" ? 11 : nil, canSubmit: canSubmit)
    }
    func prepareSend(_ request: MultiStandbySendRequest) async throws -> MultiStandbySendIntent {
        if failPreparingBefore { throw CocoaError(.fileWriteOutOfSpace) }
        if let prior = intentLedger[request.clientRequestID] {
            guard prior.request == request else { throw CocoaError(.fileReadCorruptFile) }
            return prior
        }
        let intent = makeIntent(request, phase: "prepared", state: "prepared")
        intentLedger[request.clientRequestID] = intent
        if failPreparingAfter { throw URLError(.networkConnectionLost) }
        return intent
    }
    func authorizeSend(_ request: MultiStandbySendRequest) async throws -> MultiStandbySendIntent {
        guard let prior = intentLedger[request.clientRequestID], prior.request == request else { throw CocoaError(.fileReadCorruptFile) }
        guard prior.state == "prepared" else { return prior }
        intentLedger[request.clientRequestID] = makeIntent(request, phase: "submitting", state: "unknown")
        if failAuthorizingAfter { throw URLError(.networkConnectionLost) }
        return makeIntent(request, phase: "submitting", state: "unknown", canSubmit: true)
    }
    func sendIntent(profileID: String, clientRequestID: String) async throws -> MultiStandbySendIntent {
        intentReads += 1
        guard let intent = intentLedger[clientRequestID], intent.profileID == profileID else { throw CocoaError(.fileNoSuchFile) }
        if holdIntentQueries { return try await withCheckedThrowingContinuation { intentWaiters.append((intent, $0)) } }
        return intent
    }
    func sendIntents(cursor: String?) async throws -> MultiStandbySendIntentsResponse {
        let all = intentLedger.values.filter(\.needsRecovery).sorted { $0.id < $1.id }
        let offset = Int(cursor ?? "0")!
        let end = min(all.count, offset + 100)
        return MultiStandbySendIntentsResponse(intents: Array(all[offset..<end]),
            nextCursor: end < all.count ? MultiStandbyCursor(String(end)) : nil)
    }
    func delete(messageID: String, profileID: String) async throws {
        if holdDeletes {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                deleteWaiters.append((profileID, messageID, continuation))
            }
        }
        deleted.append((profileID, messageID))
        changeRevision = max(20, changeRevision + 1)
    }
}

@main
private enum MultiStandbyTests {
    @MainActor
    static func main() async throws {
        setbuf(stdout, nil)
        _ = NSApplication.shared
        if ProcessInfo.processInfo.environment["SWITCH_TEST_CASE"] == "c1-history" {
            await historyAfterRestart()
            return
        }
        if ProcessInfo.processInfo.environment["SWITCH_TEST_CASE"] == "c1-deleted-summary" {
            await deletionSummaryFailureFencing()
            return
        }
        if ProcessInfo.processInfo.environment["SWITCH_TEST_CASE"] == "s11-body-budgets" {
            await boundedAuxiliaryBodies()
            return
        }
        if ProcessInfo.processInfo.environment["SWITCH_TEST_CASE"] == "s11-lifecycle" {
            await lifecycleBoundaryFencing()
            return
        }
        if ProcessInfo.processInfo.environment["SWITCH_TEST_CASE"] == "c6-send-recovery" {
            await sendIntentRecovery()
            return
        }
        let suite = "Switch.MultiStandby.Tests." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let unknownOperation = decode(MultiStandbyOperation.self,
            "{\"operation_id\":\"unknown-op\",\"state\":\"unknown\",\"detail\":\"result unconfirmed\"}")
        require(unknownOperation.isFinished && !unknownOperation.succeeded,
                "Rust unknown terminal was still polled or presented as success")
        defaults.set("3", forKey: "switch.multistandby.forwardingCursor")
        defaults.set(true, forKey: "switch.multistandby.forwardingSeeded")
        defaults.set(true, forKey: "switch.multistandby.forwardingEnabled")
        let api = FakeAPI()
        let attention = AttentionStore(defaults: defaults)
        var notifications: [String] = []
        let store = MultiStandbyStore(api: api, defaults: defaults, notificationSink: { message, masked in
            require(masked, "new feature must default to privacy masking")
            notifications.append(message.id)
        }, attentionStore: attention)
        store.setServiceRunning(true, schedulePolling: false)
        await store.refreshNow()
        require(api.inventoryRequests == 0, "background store must not perform physical card inventory")
        store.viewingPage = true
        await store.refreshNow()
        require(api.inventoryRequests == 1, "visible page must load its explicit card inventory")
        store.viewingPage = false
        store.selectProfile("b")
        await store.refreshMessages()
        require(!store.canSend, "unready selected profile must remain unready")
        _ = await store.send(phone: "123", body: "hello")
        require(api.sent.isEmpty && store.selectedProfileID == "b", "must never fall back to profile A")
        store.selectProfile("a")
        await store.refreshMessages()
        api.failSending = true
        let failed = await store.send(phone: "123", body: "hello")
        require(!failed, "ambiguous send must not look accepted")
        let first = api.sent.last!
        api.failSending = false
        let accepted = await store.send(phone: "123", body: "hello")
        require(!accepted && api.sent.count == 1 && api.intentReads > 0,
                "unknown outcome must be queried without submitting again")
        let freshSendAccepted = await store.send(phone: "123", body: "hello", newIntent: true)
        require(freshSendAccepted && api.sent.last?.clientRequestID != first.clientRequestID,
                "explicit new send must allocate a new intent")
        require(api.sent.allSatisfy { $0.profileID == "a" }, "send escaped selected profile")
        print("PASS selection never falls back; ambiguous sends retain their idempotency key")

        // A composer captures A, suspends on its refresh, and a notification
        // selects B. Sending its fixed intent must never use the new selection.
        for _ in 0..<10 { await Task.yield() }
        api.heldProfile = "a"
        let composedSend = Task { () -> Bool in
            let route = "a"
            await store.refreshMessages()
            return await store.send(phone: "123", body: "fixed route", expectedProfileID: route)
        }
        while api.waiters.isEmpty { await Task.yield() }
        store.selectProfile("b")
        let held = api.waiters
        api.waiters.removeAll()
        for waiter in held {
            waiter.resume(returning: MultiStandbyMessagesResponse(messages: [], nextCursor: nil, revision: 10))
        }
        let composedAccepted = await composedSend.value
        require(composedAccepted && api.sent.last?.profileID == "a" && store.selectedProfileID == "b",
                "suspended conversation send used the notification-selected line")
        let sendsBeforeUnknown = api.sent.count
        _ = await store.send(phone: "123", body: "fixed route", expectedProfileID: "unknown")
        require(api.sent.count == sendsBeforeUnknown, "unknown explicit route fell back to the selection")
        api.heldProfile = nil
        store.selectProfile("a")
        print("PASS suspended conversation sends preserve their explicit profile route")

        api.failControlling = true
        await store.retry(profileID: "b")
        let retryID = api.retries.last!.clientRequestID
        let restartedStore = MultiStandbyStore(api: api, defaults: defaults,
            notificationSink: { _, _ in }, attentionStore: attention)
        restartedStore.setServiceRunning(true, schedulePolling: false)
        restartedStore.viewingPage = true
        await restartedStore.refreshNow()
        api.failControlling = false
        await restartedStore.retry(profileID: "b")
        require(api.retries.count == 2 && api.retries.last!.clientRequestID == retryID,
                "GUI restart lost the Rust-owned request identity or called start")
        await store.control(start: true, profileIDs: ["a", "b", "a"])
        require(api.controls.last?.profileIDs == ["a", "b"] && api.controls.last?.all == nil,
                "bulk control must carry only the chosen deduplicated profiles")
        let controlsBeforeUnknown = api.controls.count
        await store.control(start: true, profileIDs: ["a", "unknown"])
        require(api.controls.count == controlsBeforeUnknown, "unknown profile escaped bulk selection")
        await store.refreshRuntimeReport(hours: 12)
        require(store.runtimeReport != nil && api.reportRequests.last?.1 == store.selectedCardID,
                "runtime report lost selected card scope")
        let stable = MultiStandbyPresentation.profiles(Array(api.profileResponse.profiles.reversed()), query: "", filter: .all, anomaliesFirst: false, masked: true)
        require(stable.map(\.profileID) == ["a", "b"], "default order depends on online transitions")
        let filtered = MultiStandbyPresentation.profiles(api.profileResponse.profiles, query: "B", filter: .recovering, anomaliesFirst: false, masked: true)
        require(filtered.map(\.profileID) == ["b"], "search/status filter selected the wrong profile")
        print("PASS explicit retry, bounded bulk selection, stable sorting and report scoping")

        // A deliberately uncancellable transport completes A after B is selected.
        for _ in 0..<10 { await Task.yield() }
        api.heldProfile = "a"
        let stale = Task { await store.refreshMessages() }
        while api.waiters.isEmpty { await Task.yield() }
        store.selectProfile("b")
        await store.refreshMessages()
        let pending = api.waiters
        api.waiters.removeAll()
        for waiter in pending {
            waiter.resume(returning: MultiStandbyMessagesResponse(
                messages: [message("old", "a")], nextCursor: nil, revision: 99))
        }
        await stale.value
        require(store.selectedProfileID == "b" && store.messages.isEmpty,
                "late A response overwrote B's inbox")
        api.heldProfile = nil
        print("PASS late profile inbox responses cannot overwrite the current selection")

        api.holdProfiles = true
        let oldProfileRequest = Task { await store.refreshProfiles() }
        while api.profileWaiters.isEmpty { await Task.yield() }
        api.holdProfiles = false
        api.profileResponse = decode(MultiStandbyProfilesResponse.self,
            "{\"profiles\":[],\"boot_id\":\"boot-2\",\"revision\":1}")
        await store.refreshProfiles()
        api.profileWaiters.removeFirst().resume(returning: decode(MultiStandbyProfilesResponse.self,
            "{\"profiles\":[{\"profile_id\":\"a\",\"card_id\":\"card\",\"sms_ready\":true}],\"boot_id\":\"boot-1\",\"revision\":999}"))
        await oldProfileRequest.value
        require(store.profiles.isEmpty, "late old-boot status replaced current state")
        print("PASS boot/request fencing rejects stale status even at a greater revision")

        // Changes include outgoing and status events: only confirmed incoming
        // received records create notifications, even while the page is closed.
        api.inbox["a"] = [message("incoming", "a"), message("sent", "a", direction: "outbound", state: "accepted")]
        api.inbox["b"] = [message("incoming", "b")]
        api.changeEvents = [change(1, "a", "incoming"), change(2, "a", "sent"), change(3, "b", "incoming")]
        api.changeRevision = 3
        store.viewingPage = false
        await store.pollChanges()
        await store.pollChanges()
        require(notifications == ["a:incoming", "b:incoming"], "background notifications mixed profiles or repeated/outgoing messages")
        require(attention.unreadSMSCount == 2, "N-wait arrivals were not counted by stable message identity")
        let handoffSnapshot = store.forwardingAdmissionSnapshot()
        require(handoffSnapshot.cursor == "3" && handoffSnapshot.seeded
                && handoffSnapshot.wasEnabled == true,
                "legacy N-wait cursor was not preserved for one-time Rust handoff")
        let reopened = MultiStandbyStore(api: api, defaults: defaults,
            notificationSink: { message, _ in notifications.append(message.id) })
        reopened.setServiceRunning(true, schedulePolling: false)
        await reopened.pollChanges()
        require(notifications.count == 2, "restart replayed prior notifications")
        reopened.requestOpenMessage(profileID: "b", messageID: "incoming")
        require(reopened.selectedProfileID == "b" && reopened.selectedMessageID == "incoming",
                "notification destination lost profile identity")
        print("PASS app-level background notifications, profile-scoped routing and persisted cursor")
        await reopened.delete(message("incoming", "a"))
        require(api.deleted.isEmpty, "cross-profile deletion was permitted")
        let cursor = decode(MultiStandbyChangesResponse.self, "{\"changes\":[],\"next_cursor\":42}")
        require(cursor.nextCursor?.value == "42", "numeric cursor compatibility lost")
        print("PASS first-load baseline, delete ownership and cursor contract")
        let archiveAPI = FakeAPI()
        archiveAPI.archiveCount = 3501
        archiveAPI.archiveDatesReversed = true
        archiveAPI.changeEvents = [change(3501, "a", "archive-3501")]
        var archiveNotifications: [String] = []
        let archive = MultiStandbyStore(api: archiveAPI, defaults: defaults, notificationSink: { message, _ in
            archiveNotifications.append(message.id)
        })
        archive.setServiceRunning(true, schedulePolling: false)
        await archive.pollChanges()
        require(archiveNotifications == ["a:archive-3501"] && archiveAPI.archiveReads == 1,
                "new notifications must jump to their durable revision beyond 3200 historical messages")
        require(defaults.string(forKey: "switch.multistandby.notificationCursor") == "3501",
                "large-history notification cursor failed to advance")
        archive.requestOpenMessage(profileID: "a", messageID: "archive-3501")
        for _ in 0..<1000 where !archive.messages.contains(where: { $0.messageID == "archive-3501" }) {
            await Task.yield()
        }
        require(archive.messages.contains(where: { $0.messageID == "archive-3501" }),
                "historical notification clicks must traverse beyond 32 pages")
        archive.setServiceRunning(false)
        print("PASS large archives retain live notifications, cursor progress and historical click routing")
        store.setServiceRunning(false)
        reopened.setServiceRunning(false)
        await historyAfterRestart()
        await conversationSnapshotsAndPaging()
        await independentDisplayAndStateFencing()
        await boundedAuxiliaryBodies()
        await lifecycleBoundaryFencing()
        await deletionSummaryFailureFencing()
        await sendIntentRecovery()
    }

    static func message(_ id: String, _ profile: String, direction: String = "inbound", state: String = "received") -> MultiStandbyMessage {
        MultiStandbyMessage(messageID: id, profileID: profile, direction: direction,
            peer: "123", body: "same text", createdAtMS: 1_000, state: state, kind: nil)
    }

    @MainActor
    static func historyAfterRestart() async {
        for count in [150, 250] {
            let suite = "Switch.MultiStandby.History." + UUID().uuidString
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            defaults.set(true, forKey: "switch.multistandby.notificationSeeded")
            defaults.set(String(count), forKey: "switch.multistandby.notificationCursor")
            defaults.set(true, forKey: "switch.multistandby.forwardingSeeded")
            defaults.set(String(count), forKey: "switch.multistandby.forwardingCursor")
            defaults.set(true, forKey: "switch.multistandby.forwardingEnabled")
            let api = FakeAPI()
            api.archiveCount = count
            api.changeRevision = UInt64(count)
            var notifications = 0
            let store = MultiStandbyStore(api: api, defaults: defaults,
                notificationSink: { _, _ in notifications += 1 })
            store.setServiceRunning(true, schedulePolling: false)
            await store.refreshSMSCenter()
            await store.pollChanges()
            require(store.inboxMessages.contains { $0.messageID == "archive-\(count)" },
                    "C1: caught-up cursor restart omitted the newest archived SMS (\(count) records)")
            require(notifications == 0,
                    "history display refresh replayed notifications")
            store.setServiceRunning(false)
        }
        print("PASS C1 latest archive snapshots survive a caught-up cursor restart (150/250)")
    }

    @MainActor
    static func lifecycleBoundaryFencing() async {
        let suite = "Switch.MultiStandby.Lifecycle." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }

        let openAPI = FakeAPI()
        let oldTarget = MultiStandbyMessage(messageID: "reused", profileID: "a", direction: "inbound",
            peer: "old-peer", body: "old generation", createdAtMS: 1, state: "received", kind: "message")
        openAPI.inbox["a"] = [oldTarget]
        let openStore = MultiStandbyStore(api: openAPI, defaults: defaults, notificationSink: { _, _ in })
        openStore.setServiceRunning(true, schedulePolling: false)
        openAPI.holdProfiles = true
        openStore.requestOpenMessage(profileID: "a", messageID: oldTarget.messageID)
        while openAPI.profileWaiters.isEmpty { await Task.yield() }
        let heldProfiles = openAPI.profileWaiters
        openAPI.profileWaiters.removeAll()
        openStore.setServiceRunning(false)
        openAPI.inbox["a"] = [MultiStandbyMessage(messageID: "reused", profileID: "a", direction: "inbound",
            peer: "new-peer", body: "new generation", createdAtMS: 2, state: "received", kind: "message")]
        openStore.setServiceRunning(true, schedulePolling: false)
        openAPI.holdProfiles = false
        for waiter in heldProfiles { waiter.resume(returning: openAPI.profileResponse) }
        for _ in 0..<500 { await Task.yield() }
        require(openStore.selectedPeer == nil,
                "a notification request from the old service generation selected a reused message in the new generation")
        openStore.setServiceRunning(false)

        let deleteAPI = FakeAPI()
        let oldVictim = MultiStandbyMessage(messageID: "reused-delete", profileID: "a", direction: "inbound",
            peer: "old-peer", body: "old deletion", createdAtMS: 1, state: "received", kind: "message")
        deleteAPI.inbox["a"] = [oldVictim]
        let deleteStore = MultiStandbyStore(api: deleteAPI, defaults: defaults, notificationSink: { _, _ in })
        deleteStore.setServiceRunning(true, schedulePolling: false)
        deleteStore.selectConversation(profileID: "a", peer: oldVictim.peer)
        await deleteStore.refreshMessages()
        deleteAPI.holdDeletes = true
        let oldDelete = Task { await deleteStore.delete(oldVictim) }
        while deleteAPI.deleteWaiters.isEmpty { await Task.yield() }
        let acceptedDelete = deleteAPI.deleteWaiters.removeFirst()
        deleteStore.setServiceRunning(false)
        let replacement = MultiStandbyMessage(messageID: "reused-delete", profileID: "a", direction: "inbound",
            peer: "new-peer", body: "new lifecycle body", createdAtMS: 2, state: "received", kind: "message")
        deleteAPI.inbox["a"] = [replacement]
        deleteStore.setServiceRunning(true, schedulePolling: false)
        deleteStore.selectConversation(profileID: "a", peer: replacement.peer)
        await deleteStore.refreshMessages()
        require(deleteStore.messages.contains(where: { $0.id == replacement.id && $0.body == replacement.body }),
                "fixture failed to load the new-generation message before the stale DELETE acknowledgement")
        deleteAPI.holdDeletes = false
        acceptedDelete.2.resume(returning: ())
        await oldDelete.value
        require(deleteStore.messages.contains(where: { $0.id == replacement.id && $0.body == replacement.body }),
                "an old DELETE acknowledgement removed a reused identity from the new service generation")
        deleteStore.setServiceRunning(false)

        let failureAPI = FakeAPI()
        let failedVictim = MultiStandbyMessage(messageID: "failed-delete", profileID: "a", direction: "inbound",
            peer: "peer", body: "failure", createdAtMS: 1, state: "received", kind: "message")
        failureAPI.inbox["a"] = [failedVictim]
        let failureStore = MultiStandbyStore(api: failureAPI, defaults: defaults, notificationSink: { _, _ in })
        failureStore.setServiceRunning(true, schedulePolling: false)
        failureStore.selectConversation(profileID: "a", peer: failedVictim.peer)
        await failureStore.refreshMessages()
        failureAPI.holdDeletes = true
        let failedDelete = Task { await failureStore.delete(failedVictim) }
        while failureAPI.deleteWaiters.isEmpty { await Task.yield() }
        let rejectedDelete = failureAPI.deleteWaiters.removeFirst()
        failureStore.setServiceRunning(false)
        failureStore.setServiceRunning(true, schedulePolling: false)
        failureAPI.holdDeletes = false
        rejectedDelete.2.resume(throwing: URLError(.networkConnectionLost))
        await failedDelete.value
        require(failureStore.actionError == nil,
                "an old DELETE error was attached to the new service generation")
        failureStore.setServiceRunning(false)
        print("PASS S11 notification and DELETE completions remain bound to their originating service generation")
    }

    @MainActor
    static func conversationSnapshotsAndPaging() async {
        let suite = "Switch.MultiStandby.Conversations." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let api = FakeAPI()
        let active = (0..<250).map { index in
            MultiStandbyMessage(messageID: String(format: "active-%03d", index), profileID: "a",
                direction: "inbound", peer: "active", body: "text", createdAtMS: 1000,
                state: "received", kind: "message")
        }
        let quiet = (0..<131).map { index in
            MultiStandbyMessage(messageID: "quiet-\(index)", profileID: "a", direction: "inbound",
                peer: "quiet-\(index)", body: "old", createdAtMS: 1, state: "received", kind: "message")
        }
        let olderQuiet = quiet.map {
            MultiStandbyMessage(messageID: $0.messageID + "-older", profileID: "a", direction: "inbound",
                peer: $0.peer, body: "older", createdAtMS: 0, state: "received", kind: "message")
        }
        api.inbox["a"] = active + quiet + olderQuiet
        api.inbox["b"] = [MultiStandbyMessage(messageID: "active-150", profileID: "b", direction: "inbound",
            peer: "active", body: "other card", createdAtMS: 1000, state: "received", kind: "message")]
        var notifications = 0
        let store = MultiStandbyStore(api: api, defaults: defaults,
            notificationSink: { _, _ in notifications += 1 })
        store.setServiceRunning(true, schedulePolling: false)
        await store.refreshSMSCenter()
        require(store.inboxConversations.count == 133 && api.summaryReads == 2 && api.historyReads == 0,
                "summary pagination lost quiet conversations or fetched complete body archives")
        require(store.inboxConversations.first { $0.peer == "active" && $0.profileID == "a" }?.messageCount == 250,
                "summary displayed the loaded page size instead of full conversation count")
        let summaryReads = api.summaryReads
        store.viewingInbox = true
        await store.refreshNow()
        require(api.summaryReads == summaryReads && api.historyReads == 0,
                "an unchanged background poll reloaded all conversation summaries/bodies")

        // Full-summary pagination starts before a deletion, while its later
        // page waits. The newer targeted summary must survive the old snapshot.
        let target = store.inboxConversations.sorted { $0.id < $1.id }.first { $0.profileID == "a" && $0.peer != "active" }!
        store.selectConversation(profileID: target.profileID, peer: target.peer)
        await store.refreshMessages()
        api.holdLaterSummaryPage = true
        api.changeCursors.removeAll()
        let staleSummaries = Task { await store.refreshSMSCenter() }
        while api.summaryWaiters.isEmpty { await Task.yield() }
        await store.delete(target.latestMessage)
        let passedCursor = store.inboxConversations.map(\.id).sorted()[99]
        let newPeer = (0..<1000).map { "new-before-\($0)" }.first {
            SMSConversationIdentity.digest("a\u{0}" + $0) < passedCursor
        }!
        let newMessage = MultiStandbyMessage(messageID: "new-before-cursor", profileID: "a", direction: "inbound",
            peer: newPeer, body: "new quiet thread", createdAtMS: 2000, state: "received", kind: "message")
        api.inbox["a", default: []].append(newMessage)
        api.changeRevision = 30
        let newConversationID = SMSConversationIdentity.digest("a\u{0}" + newPeer)
        var newChange = change(30, "a", newMessage.messageID)
        newChange.conversationID = newConversationID
        api.changeEvents = [newChange]
        let heldSummaries = api.summaryWaiters
        api.summaryWaiters.removeAll()
        api.holdLaterSummaryPage = false
        for (response, waiter) in heldSummaries {
            // Second page is from r2: its key range cannot include the newly
            // inserted lower key. Reconciliation must start at first-page r1.
            waiter.resume(returning: MultiStandbyConversationsResponse(conversations: response.conversations,
                nextCursor: response.nextCursor, revision: 30))
        }
        await staleSummaries.value
        let refreshedTarget = store.inboxConversations.first { $0.id == target.id }
        require(refreshedTarget?.messageCount == 1 && refreshedTarget?.latestMessage.messageID == target.latestMessage.messageID + "-older",
                "an older full-summary snapshot replaced a newer targeted deletion result")
        require(store.inboxConversations.contains { $0.id == newConversationID } && api.changeCursors.contains { $0 == "10" },
                "a cross-page r2 checkpoint skipped a new lower-key conversation after the r1 cursor")
        store.selectConversation(profileID: "a", peer: "active")
        await store.refreshMessages()
        require(store.messages.count == 100 && store.messages.last?.messageID == "active-249"
                && store.messages.allSatisfy { $0.peer == "active" },
                "latest peer-scoped snapshot omitted the newest records or mixed conversations")
        let anchor = store.messages.first!
        await store.delete(anchor)
        await store.refreshMessages(loadMore: true)
        await store.refreshMessages(loadMore: true)
        require(store.messages.count == 249 && Set(store.messages.map(\.id)).count == 249 && store.messageCursor == nil,
                "equal timestamps/deleted paging anchor skipped or repeated historical messages")
        require(api.inbox["b"]?.count == 1 && api.deleted.last?.0 == "a" && api.deleted.last?.1 == "active-150",
                "deleting a conversation message escaped its profile")

        // A transport returns its older snapshot after the user deletes a row.
        let victim = store.messages.last!
        api.heldProfile = "a"
        let stale = Task { await store.refreshMessages() }
        while api.waiters.isEmpty { await Task.yield() }
        let pending = api.waiters
        api.waiters.removeAll()
        api.heldProfile = nil
        await store.delete(victim)
        for waiter in pending {
            waiter.resume(returning: MultiStandbyMessagesResponse(messages: [victim], nextCursor: nil, revision: 10))
        }
        await stale.value
        require(!store.messages.contains { $0.id == victim.id } && !store.inboxMessages.contains { $0.id == victim.id },
                "a stale history response resurrected a deleted message")

        store.requestOpenMessage(profileID: "a", messageID: "active-000")
        for _ in 0..<1000 where !store.messages.contains(where: { $0.messageID == "active-000" }) { await Task.yield() }
        require(store.messages.contains { $0.messageID == "active-000" } && store.selectedPeer == "active",
                "notification navigation could not load a target outside the latest history page")
        // The view can consume a notification from its previous inbox cache
        // before the asynchronous history lookup finishes. Lookup ownership
        // must not depend on the UI's one-shot published request field.
        store.requestOpenMessage(profileID: "a", messageID: "active-001")
        store.consumeOpenMessageRequest()
        store.selectConversation(profileID: "a", peer: "active")
        for _ in 0..<1000 where !store.messages.contains(where: { $0.messageID == "active-001" }) { await Task.yield() }
        require(store.messages.contains { $0.messageID == "active-001" },
                "consuming a cached notification request cancelled its older-page lookup")
        require(notifications == 0,
                "history/summary synchronization replayed notifications")
        store.setServiceRunning(false)
        print("PASS C1 complete quiet/active summaries, peer-scoped equal-time paging, deleted-anchor/stale-response fencing and old notification targets")
    }

    @MainActor
    static func independentDisplayAndStateFencing() async {
        let suite = "Switch.MultiStandby.Display." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let api = FakeAPI()
        func sample(_ index: Int) -> MultiStandbyMessage {
            MultiStandbyMessage(messageID: String(format: "sample-%03d", index), profileID: "a", direction: "inbound",
                peer: "active", body: "text", createdAtMS: Int64(index), state: "received", kind: "message")
        }
        api.inbox["a"] = (0..<250).map(sample)
        var notifications = 0
        let store = MultiStandbyStore(api: api, defaults: defaults,
            notificationSink: { _, _ in notifications += 1 })
        store.setServiceRunning(true, schedulePolling: false)
        store.viewingInbox = true
        await store.refreshSMSCenter()
        store.selectConversation(profileID: "a", peer: "active")
        await store.refreshMessages()
        let arrived = sample(250)
        api.inbox["a", default: []].append(arrived)
        api.changeRevision = 11
        var event = change(11, "a", arrived.messageID)
        event.conversationID = SMSConversationIdentity.digest("a\u{0}active")
        api.changeEvents = [event]
        await store.pollConversationChanges()
        await store.pollChanges() // First notification poll establishes its baseline.
        require(store.messages.contains { $0.id == arrived.id } && notifications == 0,
                "notification baseline seeding suppressed visible conversation updates")

        await store.refreshMessages(loadMore: true)
        api.inbox["a", default: []] += (251..<500).map(sample)
        await store.refreshMessages()
        for _ in 0..<5 where store.messageCursor != nil { await store.refreshMessages(loadMore: true) }
        require(store.messages.count == 500 && Set(store.messages.map(\.id)).count == 500,
                "a non-overlapping latest page retained its older cursor and stranded the middle history")

        var queued = MultiStandbyMessage(messageID: "send-intent", profileID: "a", direction: "outbound",
            peer: "active", body: "outbound", createdAtMS: 600, state: "queued", kind: "message")
        queued.revision = 12
        api.inbox["a", default: []].append(queued)
        api.changeRevision = 12
        await store.refreshMessages()
        api.heldProfile = "a"
        api.holdHistoryOnly = true
        let stale = Task { await store.refreshMessages() }
        while api.waiters.isEmpty { await Task.yield() }
        var accepted = MultiStandbyMessage(messageID: "send-intent", profileID: "a", direction: "outbound",
            peer: "active", body: "outbound", createdAtMS: 600, state: "accepted", kind: "message")
        accepted.revision = 20
        api.inbox["a"]!.removeAll { $0.id == accepted.id }
        api.inbox["a"]!.append(accepted)
        api.changeRevision = 20
        var stateChange = change(20, "a", accepted.messageID)
        stateChange.conversationID = event.conversationID
        api.changeEvents = [event, stateChange]
        await store.pollConversationChanges()
        let held = api.waiters
        api.waiters.removeAll()
        api.heldProfile = nil
        api.holdHistoryOnly = false
        for waiter in held {
            waiter.resume(returning: MultiStandbyMessagesResponse(messages: [queued], nextCursor: nil, revision: 12))
        }
        await stale.value
        require(store.messages.first { $0.id == accepted.id }?.state == "accepted"
                && store.inboxMessages.first { $0.id == accepted.id }?.state == "accepted"
                && store.inboxConversations.first { $0.peer == "active" }?.latestMessage.state == "accepted",
                "an old queued history response overwrote a newer accepted state")
        let legacyDeletion = MultiStandbyChange(revision: 21, profileID: "a", messageID: "unknown-legacy-tombstone",
            direction: nil, kind: nil, state: nil, deleted: true)
        api.changeEvents.append(legacyDeletion)
        api.changeRevision = 21
        api.failSummaryBeforeCount = 1
        api.changeCursors.removeAll()
        await store.pollConversationChanges()
        await store.pollConversationChanges()
        require(api.changeCursors.filter { $0 == "20" }.count == 2,
                "failed legacy-tombstone reconciliation advanced its display checkpoint instead of retrying")
        store.setServiceRunning(false)
        print("PASS C1 display independent of notification seeding, detached latest-page reanchoring, per-message state revision fencing and failed legacy-tombstone checkpoint retry")
    }

    @MainActor
    static func boundedAuxiliaryBodies() async {
        func checkBudget(_ store: MultiStandbyStore) {
            let usage = store.auxiliaryMessageBodyUsage
            require(usage.cacheCount <= 2_048 && usage.cacheBodyBytes <= 8 * 1_024 * 1_024,
                    "auxiliary message cache exceeded its count/body budget")
            require(usage.inboxCount <= 256 && usage.inboxBodyBytes <= 1_024 * 1_024,
                    "inbox deep-link ring exceeded its count/body budget")
        }
        func row(_ id: String, peer: String, date: Int64, body: String,
                 profile: String = "a", state: String = "received", revision: UInt64 = 30) -> MultiStandbyMessage {
            var message = MultiStandbyMessage(messageID: id, profileID: profile, direction: state == "received" ? "inbound" : "outbound",
                peer: peer, body: body, createdAtMS: date, state: state, kind: "message")
            message.revision = revision
            return message
        }
        let suite = "Switch.MultiStandby.BodyBudget." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let api = FakeAPI()
        let text = String(repeating: "x", count: 4_096)
        api.changeRevision = 30
        api.inbox["a"] = (0..<2_200).map { row("history-\($0)", peer: "history", date: Int64($0), body: text) }
        api.inbox["b"] = (0..<100).map { row("other-\($0)", peer: "other", date: Int64($0), body: text, profile: "b") }
        let store = MultiStandbyStore(api: api, defaults: defaults, notificationSink: { _, _ in })
        store.setServiceRunning(true, schedulePolling: false)
        store.viewingInbox = true
        store.selectConversation(profileID: "a", peer: "history")
        await store.refreshMessages()
        for _ in 0..<30 where store.messageCursor != nil {
            await store.refreshMessages(loadMore: true)
            checkBudget(store)
        }
        require(store.messages.count == 2_200 && store.messageCursor == nil,
                "auxiliary budget silently truncated the explicitly loaded history or cursor")
        require(api.inbox["a"]?.count == 2_200 && api.deleted.isEmpty,
                "cache eviction called durable deletion")
        require(store.auxiliaryMessageBodyUsage.cacheCount == 2_048 && store.inboxMessages.count == 256,
                "fixture did not cross both auxiliary count/body limits")
        store.selectConversation(profileID: "b", peer: "other")
        await store.refreshMessages()
        require(store.messages.count == 100 && store.messages.allSatisfy { $0.profileID == "b" },
                "cross-budget profile switch retained another conversation's visible bodies")
        checkBudget(store)
        store.setServiceRunning(false)
        print("PASS S11 auxiliary count/body budgets preserve loaded history, cursor, profile scope and durable archive")

        let raceAPI = FakeAPI()
        // The critical summary sorts first, so every remaining summary can
        // evict its auxiliary body while its visible newer row stays selected.
        let peers = (0..<2_201).map { "budget-peer-\($0)" }.sorted {
            SMSConversationIdentity.digest("a\u{0}" + $0) < SMSConversationIdentity.digest("a\u{0}" + $1)
        }
        let criticalPeer = peers[0]
        var queued = row("critical", peer: criticalPeer, date: 0, body: "original payload", state: "queued", revision: 12)
        var accepted = row("critical", peer: criticalPeer, date: 0, body: "original payload", state: "accepted", revision: 20)
        // Legacy wire bodies omit a per-message revision. Their real page
        // observation still fences old queued replies after body eviction.
        queued.revision = nil
        accepted.revision = nil
        raceAPI.inbox["a"] = [queued]
        raceAPI.changeRevision = 12
        let race = MultiStandbyStore(api: raceAPI, defaults: defaults, notificationSink: { _, _ in })
        race.setServiceRunning(true, schedulePolling: false)
        race.viewingInbox = true
        await race.refreshSMSCenter()
        race.selectProfile("a")
        await race.refreshMessages()
        for _ in 0..<30 { await Task.yield() }
        raceAPI.heldProfile = "a"
        raceAPI.holdHistoryOnly = true
        let stale = Task { await race.refreshMessages() }
        while raceAPI.waiters.isEmpty { await Task.yield() }
        raceAPI.inbox["a"] = [accepted]
        raceAPI.changeRevision = 20
        let advanced = MultiStandbyChange(revision: 20, profileID: "a", messageID: "critical",
            direction: "outbound", kind: "message", state: "accepted", deleted: nil,
            conversationID: SMSConversationIdentity.digest("a\u{0}" + criticalPeer))
        raceAPI.changeEvents = [advanced]
        await race.pollConversationChanges()
        require(race.messages.first?.state == "accepted", "fixture did not publish the newer state")
        let filler = peers.dropFirst().enumerated().map {
            row("filler-\($0.offset)", peer: $0.element, date: Int64($0.offset + 1), body: text)
        }
        raceAPI.inbox["a"] = [accepted] + filler
        raceAPI.changeRevision = 30
        await race.refreshSMSCenter()
        require(race.inboxConversations.count == 2_201,
                "auxiliary budget silently truncated the full conversation summary list")
        require(!race.hasAuxiliaryCachedBody(accepted.id)
                && !race.inboxMessages.contains { $0.id == accepted.id },
                "fixture did not evict the critical body before its stale history reply")
        checkBudget(race)
        let held = raceAPI.waiters
        raceAPI.waiters.removeAll()
        raceAPI.heldProfile = nil
        raceAPI.holdHistoryOnly = false
        for waiter in held {
            waiter.resume(returning: MultiStandbyMessagesResponse(messages: [queued], nextCursor: nil, revision: 12))
        }
        await stale.value
        require(race.messages.first { $0.id == accepted.id }?.state == "accepted"
                && race.messages.first { $0.id == accepted.id }?.revision == nil
                && race.inboxConversations.first { $0.peer == criticalPeer }?.latestMessage.state == "accepted",
                "evicted-body cache miss let an old nil-revision queued page replace accepted state")
        require(raceAPI.deleted.isEmpty, "cache pressure deleted persistent messages")

        // Keep a current, uncancellable history read outstanding while an
        // explicit delete succeeds and summary reconciliation fails.
        raceAPI.heldProfile = "a"
        raceAPI.holdHistoryOnly = true
        let preDelete = Task { await race.refreshMessages() }
        while raceAPI.waiters.isEmpty { await Task.yield() }
        raceAPI.failSummaryBeforeCount = 1
        await race.delete(accepted)
        let preDeleteWaiters = raceAPI.waiters
        raceAPI.waiters.removeAll()
        raceAPI.heldProfile = nil
        raceAPI.holdHistoryOnly = false
        for waiter in preDeleteWaiters {
            waiter.resume(returning: MultiStandbyMessagesResponse(messages: [accepted], nextCursor: nil, revision: 30))
        }
        await preDelete.value
        require(!race.messages.contains { $0.id == accepted.id }
                && !race.inboxMessages.contains { $0.id == accepted.id }
                && !race.hasAuxiliaryCachedBody(accepted.id),
                "an evicted body and stale reply resurrected an explicitly deleted row")
        checkBudget(race)

        let target = filler[0]
        require(!race.inboxMessages.contains { $0.id == target.id }, "old notification target was still in the ring")
        let priorReads = raceAPI.historyReads
        race.requestOpenMessage(profileID: "a", messageID: target.messageID)
        for _ in 0..<2_000 where !race.messages.contains(where: { $0.id == target.id }) { await Task.yield() }
        require(race.messages.contains { $0.id == target.id && $0.body == target.body }
                && race.selectedPeer == target.peer && raceAPI.historyReads > priorReads,
                "an evicted old notification could not reload its original full body from the archive API")
        race.consumeOpenMessageRequest()
        for _ in 0..<30 { await Task.yield() }
        checkBudget(race)

        let huge = row("oversized", peer: "oversized-peer", date: 50_000,
                       body: String(repeating: "z", count: 9 * 1_024 * 1_024), revision: 40)
        raceAPI.inbox["a", default: []].append(huge)
        raceAPI.changeRevision = 40
        race.requestOpenMessage(profileID: "a", messageID: huge.messageID)
        for _ in 0..<2_000 where !race.messages.contains(where: { $0.id == huge.id }) { await Task.yield() }
        require(race.messages.first { $0.id == huge.id }?.body.utf8.count == huge.body.utf8.count,
                "a single oversized notification target was truncated or hidden from its selected history")
        require(!race.hasAuxiliaryCachedBody(huge.id) && !race.inboxMessages.contains { $0.id == huge.id },
                "pinning an oversized notification target bypassed an auxiliary body budget")
        require(raceAPI.inbox["a"]?.count == 2_202 && raceAPI.deleted.count == 1,
                "auxiliary eviction altered the durable fixture beyond the explicit user delete")
        for _ in 0..<30 { await Task.yield() }
        checkBudget(race)
        race.setServiceRunning(false)
        print("PASS S11 evicted-body nil-revision stale-page/deletion fences, complete summaries, old notification reload and oversized target budgets")
        await legacyEvictedNotificationReload()
    }

    @MainActor
    static func legacyEvictedNotificationReload() async {
        let suite = "Switch.MultiStandby.LegacyBodyBudget." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let api = FakeAPI()
        api.archiveCount = 2_500
        let store = MultiStandbyStore(api: api, defaults: defaults, notificationSink: { _, _ in })
        store.setServiceRunning(true, schedulePolling: false)
        store.viewingInbox = true
        await store.refreshSMSCenter()
        for profile in ["a", "b"] {
            store.selectConversation(profileID: profile, peer: "123")
            await store.refreshMessages()
            for _ in 0..<30 where store.messageCursor != nil { await store.refreshMessages(loadMore: true) }
            require(store.messages.count == 2_500 && store.messages.allSatisfy { $0.revision == nil },
                    "legacy fixture did not load a full long thread with absent business revisions")
        }
        let targetID = "archive-1"
        let identity = "a:" + targetID
        require(!store.hasAuxiliaryCachedBody(identity)
                && !store.inboxMessages.contains { $0.id == identity }
                && !store.messages.contains { $0.id == identity },
                "legacy notification target was not evicted from all auxiliary/current bodies")
        let previousArchiveReads = api.archiveReads
        store.requestOpenMessage(profileID: "a", messageID: targetID)
        for _ in 0..<2_000 where !(store.selectedPeer == "123"
            && store.messages.contains(where: { $0.id == identity })
            && store.messages.contains(where: { $0.id == "a:archive-2500" })) {
            await Task.yield()
        }
        require(store.selectedPeer == "123"
                && store.messages.contains { $0.id == identity && $0.body == "history" && $0.revision == nil }
                && api.archiveReads > previousArchiveReads,
                "nil-revision deep link lost its page observation and could not reload the old selected target")
        // The target is older than the latest 100. This assertion excludes a
        // false pass caused by an unrelated latest-history/summary refresh.
        require(store.messages.contains { $0.id == "a:archive-2500" } && store.messages.count <= 101,
                "legacy deep-link fixture did not combine the old target with only the latest history page")
        require(store.auxiliaryMessageBodyUsage.cacheCount <= 2_048
                && store.auxiliaryMessageBodyUsage.inboxCount <= 256 && api.deleted.isEmpty,
                "legacy deep-link reload exceeded a count budget or deleted durable history")
        for _ in 0..<30 { await Task.yield() }
        store.setServiceRunning(false)
        print("PASS S11 nil-revision old notification target reloads after eviction from a 2500-message thread without forging a business revision")
    }

    @MainActor
    static func deletionSummaryFailureFencing() async {
        for remote in [false, true] {
            let suite = "Switch.MultiStandby.DeletedSummary." + UUID().uuidString
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let api = FakeAPI()
            let older = MultiStandbyMessage(messageID: "older", profileID: "a", direction: "inbound",
                peer: "123", body: "remaining older body", createdAtMS: 500, state: "received", kind: "message")
            let victim = MultiStandbyMessage(messageID: "victim", profileID: "a", direction: "inbound",
                peer: "123", body: "deleted latest body", createdAtMS: 1000, state: "received", kind: "message")
            api.inbox["a"] = [older, victim]
            api.inbox["b"] = [MultiStandbyMessage(messageID: "victim", profileID: "b", direction: "inbound",
                peer: "123", body: "other profile body", createdAtMS: 1000, state: "received", kind: "message")]
            let store = MultiStandbyStore(api: api, defaults: defaults,
                notificationSink: { _, _ in })
            store.setServiceRunning(true, schedulePolling: false)
            await store.refreshSMSCenter()
            store.selectConversation(profileID: "a", peer: "123")
            await store.refreshMessages()
            api.holdAllSummaryPages = true
            let stale = Task { await store.refreshSMSCenter() }
            while api.summaryWaiters.isEmpty { await Task.yield() }
            api.failSummaryBeforeCount = 2
            if remote {
                api.deleted.append(("a", "victim"))
                api.changeRevision = 21
                api.changeEvents = [MultiStandbyChange(revision: 21, profileID: "a", messageID: "victim",
                    direction: "inbound", kind: "message", state: "received", deleted: true,
                    conversationID: SMSConversationIdentity.digest("a\u{0}123"))]
                await store.pollConversationChanges()
            } else {
                await store.delete(victim)
                require(api.deleted.last?.0 == "a" && api.deleted.last?.1 == "victim", "fixture DELETE was not accepted")
                require(store.actionError == nil && store.lastError?.contains("短信已删除，会话同步稍后重试") == true,
                        "a confirmed DELETE was presented as failed when only its summary refresh failed")
            }
            if store.inboxConversations.contains(where: { $0.latestMessage.id == victim.id }) {
                print("FAIL C1 deleted latest body remains in conversation summary after \(remote ? "remote delete" : "accepted DELETE") and summary read failure")
                exit(1)
            }
            let pending = api.summaryWaiters
            api.summaryWaiters.removeAll()
            api.holdAllSummaryPages = false
            for (response, waiter) in pending { waiter.resume(returning: response) }
            await stale.value
            require(!store.inboxConversations.contains { $0.latestMessage.id == victim.id }
                    && !store.inboxMessages.contains { $0.id == victim.id }
                    && !store.messages.contains { $0.id == victim.id },
                    "a late full-summary response restored the deleted latest body")
            require(store.inboxConversations.contains { $0.latestMessage.id == "b:victim" },
                    "deleted-summary removal escaped its profile")
            api.failSummaryBeforeCount = 0
            await store.refreshSMSCenter()
            let restored = store.inboxConversations.first { $0.profileID == "a" }
            require(restored?.messageCount == 1 && restored?.latestMessage.id == older.id,
                    "successful reconciliation did not restore the accurate remaining conversation")
            store.setServiceRunning(false)
        }
        print("PASS C1 accepted/remote deletion removes failed-refresh summaries, rejects late deleted latest bodies and restores only authoritative counts")
    }
    @MainActor
    static func sendIntentRecovery() async {
        let suite = "Switch.MultiStandby.SendRecovery." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let api = FakeAPI()
        let original = MultiStandbyStore(api: api, defaults: defaults,
            notificationSink: { _, _ in })
        original.setServiceRunning(true, schedulePolling: false)
        await original.refreshNow()
        original.selectProfile("a")
        api.failSending = true
        _ = await original.send(phone: "123", body: "fixed intent", expectedProfileID: "a")
        require(api.sent.count == 1, "fixture did not reach the send boundary")
        original.setServiceRunning(false)
        let reopened = MultiStandbyStore(api: api, defaults: defaults,
            notificationSink: { _, _ in })
        reopened.setServiceRunning(true, schedulePolling: false)
        await reopened.refreshNow()
        reopened.selectProfile("a")
        api.failSending = false
        _ = await reopened.send(phone: "123", body: "fixed intent", expectedProfileID: "a")
        if api.sent.count != 1 {
            print("FAIL C6 restart loses send intent: the same unconfirmed action formed a second request ID")
            exit(1)
        }
        reopened.setServiceRunning(false)
        print("PASS C6 restart queries an unconfirmed intent without replaying it")

        // A durable preparation that never reached submission remains safe to
        // continue explicitly using its original ID after a store restart.
        let preparedAPI = FakeAPI()
        preparedAPI.failPreparingAfter = true
        let preparedStore = MultiStandbyStore(api: preparedAPI, defaults: defaults,
            notificationSink: { _, _ in })
        preparedStore.setServiceRunning(true, schedulePolling: false)
        await preparedStore.refreshNow()
        let saved = await preparedStore.send(phone: "456", body: "prepared body", expectedProfileID: "a")
        require(!saved && preparedAPI.sent.isEmpty && preparedStore.unresolvedSends.first?.state == "prepared",
                "lost preparation response crossed the queue boundary or became unknown")
        let fixed = preparedStore.unresolvedSends.first!
        preparedStore.setServiceRunning(false)
        preparedAPI.failPreparingAfter = false
        let preparedReopened = MultiStandbyStore(api: preparedAPI, defaults: defaults,
            notificationSink: { _, _ in })
        preparedReopened.setServiceRunning(true, schedulePolling: false)
        await preparedReopened.refreshNow()
        require(preparedAPI.sent.isEmpty && preparedReopened.unresolvedSends.first?.state == "prepared",
                "restart automatically submitted a prepared intent")
        preparedAPI.holdIntentQueries = true
        let oldQuery = Task { await preparedReopened.querySendIntent(fixed) }
        while preparedAPI.intentWaiters.isEmpty { await Task.yield() }
        preparedReopened.selectProfile("b")
        let continued = await preparedReopened.continueSendIntent(fixed)
        require(continued && preparedAPI.sent.last == fixed.request && preparedReopened.selectedProfileID == "b",
                "explicit prepared continuation changed its profile, payload or request ID")
        let oldResponses = preparedAPI.intentWaiters
        preparedAPI.intentWaiters.removeAll()
        preparedAPI.holdIntentQueries = false
        for (response, waiter) in oldResponses { waiter.resume(returning: response) }
        _ = await oldQuery.value
        require(preparedReopened.unresolvedSends.isEmpty,
                "a stale prepared query resurrected an already queued send intent")
        preparedReopened.setServiceRunning(false)
        print("PASS C6 durable prepared intent continues only on explicit action with fixed profile and payload")

        let authorizationAPI = FakeAPI()
        authorizationAPI.failAuthorizingAfter = true
        let authorizationStore = MultiStandbyStore(api: authorizationAPI, defaults: defaults,
            notificationSink: { _, _ in })
        authorizationStore.setServiceRunning(true, schedulePolling: false)
        await authorizationStore.refreshNow()
        _ = await authorizationStore.send(phone: "789", body: "authorization response lost", expectedProfileID: "a")
        require(authorizationAPI.sent.isEmpty && authorizationStore.unresolvedSends.first?.state == "unknown",
                "missing authorization acknowledgement permitted a queue call")
        let unknown = authorizationStore.unresolvedSends.first!
        authorizationAPI.failAuthorizingAfter = false
        _ = await authorizationStore.send(phone: unknown.phone, body: unknown.body, expectedProfileID: unknown.profileID)
        require(authorizationAPI.sent.isEmpty, "repeated unknown action replayed submission authorization")
        let explicitNew = await authorizationStore.send(phone: unknown.phone, body: unknown.body,
            expectedProfileID: unknown.profileID, newIntent: true)
        require(explicitNew && authorizationAPI.sent.count == 1 && authorizationAPI.sent[0].clientRequestID != unknown.clientRequestID,
                "old unknown intent blocked an explicitly new request")
        authorizationStore.setServiceRunning(false)
        print("PASS C6 lost one-time authorization pauses, query does not replay, explicit new request remains available")

        let committedAPI = FakeAPI()
        committedAPI.failSending = true
        committedAPI.queueBeforeSendError = true
        let committedStore = MultiStandbyStore(api: committedAPI, defaults: defaults,
            notificationSink: { _, _ in })
        committedStore.setServiceRunning(true, schedulePolling: false)
        await committedStore.refreshNow()
        let known = await committedStore.send(phone: "111", body: "queue ack lost", expectedProfileID: "a")
        require(known && committedAPI.sent.count == 1 && committedStore.unresolvedSends.isEmpty,
                "durably queued response loss was not recovered by read-only evidence")
        require(committedStore.sendNotice?.contains("等待发送") == true && committedStore.sendNotice?.contains("已送达") == false,
                "queue evidence became end-to-end delivery")
        committedStore.setServiceRunning(false)
        print("PASS C6 committed queue response loss queries real state and never implies delivered")

        let terminalAPI = FakeAPI()
        terminalAPI.failSending = true
        terminalAPI.queueBeforeSendError = true
        terminalAPI.sendOutcomeState = "failed"
        let terminalStore = MultiStandbyStore(api: terminalAPI, defaults: defaults,
            notificationSink: { _, _ in })
        terminalStore.setServiceRunning(true, schedulePolling: false)
        await terminalStore.refreshNow()
        let terminalAccepted = await terminalStore.send(phone: "112", body: "terminal failure", expectedProfileID: "a")
        let failedID = terminalAPI.sent[0].clientRequestID
        require(!terminalAccepted && terminalStore.unresolvedSends.isEmpty && terminalStore.sendNotice?.contains("发送失败") == true,
                "known failed evidence remained unknown after a lost send response")
        terminalAPI.failSending = false
        terminalAPI.sendOutcomeState = "queued"
        let afterFailure = await terminalStore.send(phone: "112", body: "terminal failure", expectedProfileID: "a")
        require(afterFailure && terminalAPI.sent.count == 2 && terminalAPI.sent[1].clientRequestID != failedID,
                "known terminal request ID prevented a later explicit send from creating a fresh request")
        terminalStore.setServiceRunning(false)
        print("PASS C6 known failed result retires only its old ID so a later user send starts a fresh request")

        let failedSaveAPI = FakeAPI()
        failedSaveAPI.failPreparingBefore = true
        let failedSave = MultiStandbyStore(api: failedSaveAPI, defaults: defaults,
            notificationSink: { _, _ in })
        failedSave.setServiceRunning(true, schedulePolling: false)
        await failedSave.refreshNow()
        _ = await failedSave.send(phone: "222", body: "must not submit", expectedProfileID: "a")
        require(failedSaveAPI.sent.isEmpty && failedSaveAPI.intentLedger.isEmpty,
                "failed durable preparation called the queue endpoint")
        let preferenceData = try! PropertyListSerialization.data(fromPropertyList: defaults.persistentDomain(forName: suite) ?? [:], format: .xml, options: 0)
        let preferenceText = String(decoding: preferenceData, as: UTF8.self)
        require(!preferenceText.contains("fixed intent") && !preferenceText.contains("prepared body") && !preferenceText.contains("must not submit"),
                "send body escaped into UserDefaults")
        failedSave.setServiceRunning(false)
        print("PASS C6 failed persistence prevents queue submission and intent bodies stay out of preferences")

        let pagedAPI = FakeAPI()
        for index in 0..<151 {
            _ = try! await pagedAPI.prepareSend(MultiStandbySendRequest(profileID: "a", phone: "333", body: "saved \(index)", clientRequestID: "saved-\(index)"))
        }
        let pagedStore = MultiStandbyStore(api: pagedAPI, defaults: defaults,
            notificationSink: { _, _ in })
        pagedStore.setServiceRunning(true, schedulePolling: false)
        await pagedStore.refreshNow()
        require(pagedStore.unresolvedSends.count == 151 && pagedAPI.sent.isEmpty,
                "recovery list lost later pages or automatically submitted saved intents")
        pagedStore.setServiceRunning(false)
        print("PASS C6 restores all 151 intents through bounded pages without queue calls")
    }
    static func change(_ revision: UInt64, _ profile: String, _ message: String) -> MultiStandbyChange {
        MultiStandbyChange(revision: revision, profileID: profile, messageID: message, direction: nil, kind: nil, state: nil, deleted: nil)
    }
}
