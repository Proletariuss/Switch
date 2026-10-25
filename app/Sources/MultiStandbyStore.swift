import AppKit
import Combine
import Foundation
import UserNotifications

/// Owned for the lifetime of the application, independently of the selected
/// page. No legacy SMS device selection or SMSStore state is read or changed.
@MainActor
final class MultiStandbyStore: ObservableObject {
    static let shared = MultiStandbyStore(backend: BackendProcess.shared)
    typealias NotificationSink = (MultiStandbyMessage, Bool) async throws -> Void

    @Published private(set) var cards: [MultiStandbyCard] = []
    @Published private(set) var profiles: [MultiStandbyProfile] = []
    @Published private(set) var messages: [MultiStandbyMessage] = []
    /// Summaries cover every conversation without loading its full body history.
    @Published private(set) var inboxProfiles: [MultiStandbyProfile] = []
    @Published private(set) var inboxConversations: [MultiStandbyConversation] = []
    @Published private(set) var inboxMessages: [MultiStandbyMessage] = []
    @Published private(set) var selectedCardID = ""
    @Published private(set) var selectedProfileID = ""
    @Published private(set) var selectedPeer: String?
    @Published private(set) var selectedMessageID: String?
    @Published private(set) var isServiceRunning = false
    @Published private(set) var refreshing = false
    @Published private(set) var controlling = false
    @Published private(set) var sending = false
    @Published private(set) var lastError: String?
    @Published private(set) var actionError: String?
    @Published private(set) var sendNotice: String?
    @Published private(set) var unresolvedSends: [MultiStandbySendIntent] = []
    @Published private(set) var sendRecoveryLoading = false
    @Published private(set) var operation: MultiStandbyOperation?
    @Published private(set) var runtimeReport: MultiStandbyRuntimeReport?
    @Published private(set) var runtimeReportLoading = false
    @Published private(set) var runtimeReportError: String?
    @Published private(set) var messageCursor: String?
    @Published private(set) var messagesLoading = false
    @Published var viewingPage = false
    @Published var viewingInbox = false
    @Published private(set) var shouldOpenInbox = false
    @Published var notificationsEnabled: Bool {
        didSet { defaults.set(notificationsEnabled, forKey: Self.notificationsKey) }
    }

    private static let cursorKey = "switch.multistandby.notificationCursor"
    private static let seededKey = "switch.multistandby.notificationSeeded"
    private static let notifiedKey = "switch.multistandby.notifiedMessages"
    private static let forwardingCursorKey = "switch.multistandby.forwardingCursor"
    private static let forwardingSeededKey = "switch.multistandby.forwardingSeeded"
    private static let forwardingEnabledKey = "switch.multistandby.forwardingEnabled"
    private static let notificationsKey = "switch.multistandby.notificationsEnabled"
    private let api: any MultiStandbyAPI
    private let defaults: UserDefaults
    private let notificationSink: NotificationSink
    private let attentionStore: AttentionStore
    private var reportRequest: UInt64 = 0
    private var cancellables = Set<AnyCancellable>()
    private var timer: Timer?
    private var pollTask: Task<Void, Never>?
    private var lifecycle: UInt64 = 0
    private var profileRequest: UInt64 = 0
    private var messageRequest: UInt64 = 0
    private struct HistoryReadIntent: Equatable {
        let profile: String
        let peer: String?
        let epoch: UInt64
        let request: UInt64
        let cursor: String?
        let loadMore: Bool
    }
    private struct HistoryReadOwner {
        let intent: HistoryReadIntent
        let task: Task<Void, Never>
    }
    private enum HistoryReadWait {
        case active(Task<Void, Never>)
        case pending(Task<Task<Void, Never>?, Never>)
    }
    // Two actual API owners preserve selection responsiveness. Saturation
    // retains only the newest intent and one shared admission task.
    private var activeHistoryReads: [UInt64: HistoryReadOwner] = [:]
    private var pendingHistoryRead: HistoryReadIntent?
    private var pendingHistoryTask: Task<Task<Void, Never>?, Never>?
    private var historyCapacityWaiter: CheckedContinuation<Void, Never>?
    private var inboxRequest: UInt64 = 0
    private var completedInboxRequest: UInt64 = 0
    private var openMessageRequest: UInt64 = 0
    private var deleteRequest: UInt64 = 0
    private var activeDeleteRequests: [String: UInt64] = [:]
    private var summaryChangesCursor: String?
    private var summaryChangesInFlight = false
    private var conversationRevisions: [String: UInt64] = [:]
    private var deletedMessageIDs = Set<String>()
    // These budgets cover auxiliary UTF-8 bodies, not the user's explicitly
    // loaded history, all conversation summaries, or durable archive contents.
    private static let maximumCachedMessages = 2_048
    private static let maximumCachedBodyBytes = 8 * 1_024 * 1_024
    private static let maximumInboxMessages = 256
    private static let maximumInboxBodyBytes = 1_024 * 1_024
    private struct CachedMessage {
        let message: MultiStandbyMessage
        let revision: UInt64
        let bodyBytes: Int
    }
    private var messageCache: [String: CachedMessage] = [:]
    private var messageCacheOrder: [String] = []
    private var messageCacheBodyBytes = 0
    // A body eviction must not make a stale response authoritative again.
    // Revision/deletion metadata stays independent of these body budgets.
    private var messageRevisionFloors: [String: UInt64] = [:]
    private var latestSnapshotIDs = Set<String>()
    private var profileRevision: UInt64 = 0
    private var messageRevision: UInt64 = 0
    private var hasLoadedMessages = false
    private var bootID: String?
    private var changesCursor: String?
    private var forwardingChangesCursor: String?
    private var notificationsSeeded: Bool
    private var forwardingCursorSeeded: Bool
    private var forwardingWasEnabled: Bool?
    private var notifiedIDs: [String]
    private struct SendKey: Hashable {
        let profileID: String
        let phone: String
        let body: String
    }
    private var pendingSends: [SendKey: MultiStandbySendRequest] = [:]
    private var sendRecoveryLoaded = false
    private var sendRecoveryRequest: UInt64 = 0
    private var changesInFlight = false

    init(backend: BackendProcess? = nil, api: (any MultiStandbyAPI)? = nil,
         defaults: UserDefaults = .standard, notificationSink: NotificationSink? = nil,
         attentionStore: AttentionStore? = nil) {
        self.api = api ?? MultiStandbyHTTPAPI()
        self.defaults = defaults
        self.notificationSink = notificationSink ?? Self.postNotification
        self.attentionStore = attentionStore ?? .shared
        changesCursor = defaults.string(forKey: Self.cursorKey)
        forwardingChangesCursor = defaults.string(forKey: Self.forwardingCursorKey)
        notificationsSeeded = defaults.bool(forKey: Self.seededKey)
        forwardingCursorSeeded = defaults.bool(forKey: Self.forwardingSeededKey)
        forwardingWasEnabled = defaults.object(forKey: Self.forwardingEnabledKey) as? Bool
        notifiedIDs = defaults.stringArray(forKey: Self.notifiedKey) ?? []
        notificationsEnabled = defaults.object(forKey: Self.notificationsKey) as? Bool ?? true
        backend?.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in self?.setServiceRunning(state == .running) }
            .store(in: &cancellables)
    }

    deinit { timer?.invalidate(); pollTask?.cancel() }

    /// Read-only accounting for offline acceptance. Strings may share backing
    /// storage with published rows; these are retained UTF-8 payload budgets.
    var auxiliaryMessageBodyUsage: (cacheCount: Int, cacheBodyBytes: Int, inboxCount: Int, inboxBodyBytes: Int) {
        (messageCache.count, messageCacheBodyBytes, inboxMessages.count,
         inboxMessages.reduce(0) { $0 + $1.body.utf8.count })
    }

    func hasAuxiliaryCachedBody(_ identity: String) -> Bool { messageCache[identity] != nil }

    var selectedProfile: MultiStandbyProfile? {
        profiles.first { $0.profileID == selectedProfileID }
            ?? inboxProfiles.first { $0.profileID == selectedProfileID }
    }
    var onlineCount: Int { isServiceRunning ? profiles.filter(\.isReady).count : 0 }
    var canSend: Bool { isServiceRunning && selectedProfile?.isReady == true && !sending }
    var selectedCardAvailable: Bool { cards.contains { $0.cardID == selectedCardID && $0.available } }

    /// Read the legacy N-wait forwarding cursor for the one-time Rust handoff.
    /// After Rust accepts the handoff, only Rust advances its durable cursor.
    func forwardingAdmissionSnapshot() -> NwaitSMSForwardingHandoff {
        NwaitSMSForwardingHandoff(
            cursor: forwardingChangesCursor,
            seeded: forwardingCursorSeeded,
            wasEnabled: forwardingWasEnabled)
    }

    func setServiceRunning(_ running: Bool, schedulePolling: Bool = true) {
        guard isServiceRunning != running else { return }
        isServiceRunning = running
        lifecycle &+= 1
        sendRecoveryRequest &+= 1
        sendRecoveryLoaded = false
        sendRecoveryLoading = false
        profileRequest &+= 1
        messageRequest &+= 1
        activeDeleteRequests.removeAll()
        pollTask?.cancel()
        pollTask = nil
        // Keep the active history owner until its API call actually returns.
        // A stopped service must not issue its superseded pending selection.
        pendingHistoryRead = nil
        wakeHistoryAdmission()
        refreshing = false
        messagesLoading = false
        timer?.invalidate()
        timer = nil
        if running && schedulePolling {
            timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
                Task { @MainActor [weak self] in self?.refresh() }
            }
            refresh()
        }
    }

    func refresh() {
        guard isServiceRunning, pollTask == nil else { return }
        pollTask = Task { [weak self] in
            guard let self else { return }
            let epoch = self.lifecycle
            await self.refreshNow()
            if self.lifecycle == epoch { self.pollTask = nil }
        }
    }

    func refreshNow() async {
        guard isServiceRunning, !refreshing else { return }
        refreshing = true
        let epoch = lifecycle
        defer { if epoch == lifecycle { refreshing = false } }
        lastError = nil
        // Inventory can perform card I/O. Background notifications only need
        // durable snapshots and changes, so leave the legacy reader untouched
        // when this page is not in use.
        if viewingPage {
            do {
                let result = try await api.cards()
                guard current(epoch) else { return }
                cards = result.cards
                if let detail = result.detail, !detail.isEmpty { lastError = detail }
            } catch {
                guard current(epoch) else { return }
                lastError = userFacingErrorMessage(error)
            }
        }
        await refreshProfiles()
        guard current(epoch) else { return }
        if !sendRecoveryLoaded { await refreshSendIntents() }
        guard current(epoch) else { return }
        if viewingInbox { await pollConversationChanges() }
        guard current(epoch) else { return }
        if let operation, !operation.isFinished {
            do {
                let result = try await api.operation(id: operation.operationID)
                guard current(epoch), self.operation?.operationID == result.operationID else { return }
                self.operation = result
                if result.state == "unknown" {
                    actionError = result.detail ?? "操作已结束，但执行结果无法确认；请核对线路状态后再决定是否发起新操作。"
                }
            } catch {
                if current(epoch) { lastError = userFacingErrorMessage(error) }
            }
        }
        await pollChanges()
    }

    func selectCard(_ id: String) {
        guard selectedCardID != id else { return }
        selectedCardID = id
        reportRequest &+= 1
        runtimeReport = nil
        runtimeReportError = nil
        runtimeReportLoading = false
        selectedProfileID = ""
        pendingHistoryRead = nil
        wakeHistoryAdmission()
        messagesLoading = false
        selectedPeer = nil
        selectedMessageID = nil
        profiles = []
        messages = []
        messageCursor = nil
        profileRevision = 0
        messageRevision = 0
        hasLoadedMessages = false
        latestSnapshotIDs.removeAll()
        profileRequest &+= 1
        messageRequest &+= 1
        Task { await refreshProfiles() }
    }

    func selectProfile(_ id: String) {
        selectConversation(profileID: id, peer: nil)
    }

    func selectConversation(profileID id: String, peer: String?) {
        guard selectedProfileID != id || selectedPeer != peer else { return }
        selectedProfileID = id
        selectedPeer = peer
        selectedMessageID = nil
        messages = []
        messageCursor = nil
        messageRevision = 0
        hasLoadedMessages = false
        latestSnapshotIDs.removeAll()
        messagesLoading = false
        messageRequest &+= 1
        sendNotice = nil
        // An uncertain intent remains bound to its original profile/payload.
        enqueueHistoryRead(loadMore: false)
    }

    func requestOpenMessage(profileID: String, messageID: String) {
        openMessageRequest &+= 1
        let request = openMessageRequest
        selectCard("")
        selectProfile(profileID)
        selectedMessageID = messageID
        let epoch = lifecycle
        Task {
            guard current(epoch), openMessageRequest == request,
                  selectedProfileID == profileID else { return }
            await refreshProfiles()
            guard current(epoch), openMessageRequest == request,
                  selectedProfileID == profileID else { return }
            await refreshMessages()
            guard current(epoch), openMessageRequest == request,
                  selectedProfileID == profileID else { return }
            let change = MultiStandbyChange(revision: 0, profileID: profileID, messageID: messageID,
                                           direction: nil, kind: nil, state: nil, deleted: nil)
            if let found = try? await findMessage(change), current(epoch), selectedProfileID == profileID,
               openMessageRequest == request, (selectedPeer == nil || selectedPeer == found.message.peer),
               !deletedMessageIDs.contains(found.message.id) {
                let message = found.message
                selectConversation(profileID: profileID, peer: message.peer)
                selectedMessageID = messageID
                mergeMessages([message], revision: found.observationRevision)
                try? await refreshConversation(id: SMSConversationIdentity.digest(profileID + "\u{0}" + message.peer), epoch: epoch)
                guard current(epoch), openMessageRequest == request,
                      selectedProfileID == profileID, selectedPeer == message.peer else { return }
                await refreshMessages()
            }
        }
    }

    func consumeOpenMessageRequest() {
        selectedMessageID = nil
    }

    func requestSMSInbox() {
        shouldOpenInbox = true
    }

    func consumeSMSInboxRequest() {
        shouldOpenInbox = false
    }

    func refreshProfiles() async {
        guard isServiceRunning else { return }
        let epoch = lifecycle
        let card = selectedCardID
        profileRequest &+= 1
        let request = profileRequest
        do {
            let result = try await api.profiles(cardID: card)
            guard current(epoch), request == profileRequest, card == selectedCardID else { return }
            if bootID == result.bootID && result.revision < profileRevision { return }
            bootID = result.bootID
            profileRevision = result.revision
            profiles = result.profiles.filter { card.isEmpty || $0.cardID == card }
            // Preserve the selected ID even when unavailable; never fall back
            // to a different sender because a status response removed it.
        } catch {
            if current(epoch), request == profileRequest { lastError = userFacingErrorMessage(error) }
        }
    }

    func refreshMessages(loadMore: Bool = false) async {
        // Wait for an active latest page before capturing its next cursor.
        if loadMore, let latest = activeHistoryReads.values.first(where: {
            !$0.intent.loadMore && $0.intent.request == messageRequest
                && $0.intent.epoch == lifecycle && $0.intent.profile == selectedProfileID
                && $0.intent.peer == selectedPeer
        }) {
            let profile = selectedProfileID
            let peer = selectedPeer
            let epoch = lifecycle
            await latest.task.value
            guard current(epoch), profile == selectedProfileID, peer == selectedPeer else { return }
        }
        switch enqueueHistoryRead(loadMore: loadMore) {
        case .active(let owner): await owner.value
        case .pending(let admission):
            if let owner = await admission.value { await owner.value }
        case nil: break
        }
    }

    @discardableResult
    private func enqueueHistoryRead(loadMore: Bool) -> HistoryReadWait? {
        guard isServiceRunning, !selectedProfileID.isEmpty else {
            pendingHistoryRead = nil
            wakeHistoryAdmission()
            return nil
        }
        let cursor = loadMore ? messageCursor : nil
        if loadMore && cursor == nil { return nil }
        let sameRead: (HistoryReadIntent) -> Bool = { intent in
            intent.profile == self.selectedProfileID && intent.peer == self.selectedPeer
                && intent.epoch == self.lifecycle && intent.request == self.messageRequest
                && intent.cursor == cursor && intent.loadMore == loadMore
        }
        if let pending = pendingHistoryRead, sameRead(pending), let task = pendingHistoryTask {
            return .pending(task)
        }
        if loadMore, let active = activeHistoryReads.values.first(where: { sameRead($0.intent) }) {
            return .active(active.task)
        }
        messageRequest &+= 1
        let intent = HistoryReadIntent(profile: selectedProfileID, peer: selectedPeer,
            epoch: lifecycle, request: messageRequest, cursor: cursor, loadMore: loadMore)
        messagesLoading = true
        if activeHistoryReads.count < 2 && pendingHistoryTask == nil {
            return .active(startHistoryRead(intent))
        }
        pendingHistoryRead = intent
        if pendingHistoryTask == nil {
            pendingHistoryTask = Task { [weak self] in
                guard let self else { return nil }
                return await self.admitPendingHistoryRead()
            }
        }
        wakeHistoryAdmission()
        return .pending(pendingHistoryTask!)
    }

    private func startHistoryRead(_ intent: HistoryReadIntent) -> Task<Void, Never> {
        precondition(activeHistoryReads.count < 2)
        let task = Task { [weak self] in
            guard let self else { return }
            await self.performHistoryRead(intent)
            self.activeHistoryReads.removeValue(forKey: intent.request)
            self.wakeHistoryAdmission()
        }
        activeHistoryReads[intent.request] = HistoryReadOwner(intent: intent, task: task)
        return task
    }

    private func admitPendingHistoryRead() async -> Task<Void, Never>? {
        while pendingHistoryRead != nil && activeHistoryReads.count >= 2 {
            await withCheckedContinuation { historyCapacityWaiter = $0 }
        }
        let intent = pendingHistoryRead
        pendingHistoryRead = nil
        pendingHistoryTask = nil
        guard let intent, current(intent.epoch), intent.request == messageRequest,
              intent.profile == selectedProfileID, intent.peer == selectedPeer else { return nil }
        return startHistoryRead(intent)
    }

    private func wakeHistoryAdmission() {
        historyCapacityWaiter?.resume()
        historyCapacityWaiter = nil
    }

    private func performHistoryRead(_ intent: HistoryReadIntent) async {
        defer { if intent.request == messageRequest { messagesLoading = false } }
        do {
            let result = try await api.history(profileID: intent.profile, peer: intent.peer, cursor: intent.cursor)
            guard current(intent.epoch), intent.request == messageRequest, intent.profile == selectedProfileID, intent.peer == selectedPeer,
                  result.revision >= messageRevision else { return }
            messageRevision = result.revision
            let incoming = result.messages.filter { $0.profileID == intent.profile && (intent.peer == nil || $0.peer == intent.peer) }
            let incomingIDs = Set(incoming.map(\.id))
            let detachedLatestPage = !intent.loadMore && !incomingIDs.isEmpty && latestSnapshotIDs.isDisjoint(with: incomingIDs)
            mergeMessages(incoming, revision: result.revision)
            if intent.loadMore || !hasLoadedMessages || detachedLatestPage {
                let next = result.nextCursor?.value
                messageCursor = next?.isEmpty == false && next != intent.cursor ? next : nil
            }
            if !intent.loadMore { latestSnapshotIDs = incomingIDs }
            hasLoadedMessages = true
        } catch {
            if current(intent.epoch), intent.request == messageRequest { lastError = userFacingErrorMessage(error) }
        }
    }

    /// Loads all summary pages once on entry/manual refresh. Only the selected
    /// conversation loads body pages; later polls reconcile affected summaries.
    func refreshSMSCenter() async {
        guard isServiceRunning else { return }
        inboxRequest &+= 1
        let request = inboxRequest
        let epoch = lifecycle
        do {
            let result = try await api.profiles(cardID: "")
            guard current(epoch), request == inboxRequest else { return }
            inboxProfiles = result.profiles
            var cursor: String?
            var firstRevision: UInt64?
            var snapshots: [(MultiStandbyConversation, UInt64)] = []
            repeat {
                let page = try await api.conversations(profileID: nil, conversationID: nil, cursor: cursor)
                guard current(epoch), request == inboxRequest else { return }
                if firstRevision == nil { firstRevision = page.revision }
                snapshots += page.conversations.map { ($0, page.revision) }
                let next = page.nextCursor?.value
                guard next == nil || next != cursor else { throw URLError(.cannotParseResponse) }
                cursor = next
            } while cursor != nil
            let present = Set(snapshots.map { $0.0.id })
            let boundary = firstRevision ?? 0
            // A newer per-thread response may complete while the full snapshot
            // awaits another page. Never overwrite/remove that newer result.
            for existing in inboxConversations where !present.contains(existing.id)
                && (conversationRevisions[existing.id] ?? 0) <= boundary {
                applyConversation(nil, id: existing.id, revision: boundary)
            }
            for (conversation, revision) in snapshots {
                applyConversation(conversation, id: conversation.id, revision: revision)
            }
            if summaryChangesCursor == nil { summaryChangesCursor = String(boundary) }
            completedInboxRequest = request
            await pollConversationChanges()
        } catch {
            if current(epoch), request == inboxRequest {
                lastError = "短信中心同步失败：" + userFacingErrorMessage(error)
            }
        }
    }

    private func applyConversation(_ conversation: MultiStandbyConversation?, id: String, revision: UInt64) {
        guard revision >= (conversationRevisions[id] ?? 0) else { return }
        if let conversation, deletedMessageIDs.contains(conversation.latestMessage.id) { return }
        // After an auxiliary body eviction, an older response may have no
        // retained body to substitute. Keep the current summary in that case;
        // falling back to the incoming body would undo its revision fence.
        let latest: MultiStandbyMessage?
        if let conversation {
            guard let resolved = resolveMessages([conversation.latestMessage], revision: revision).first else { return }
            latest = resolved
        } else {
            latest = nil
        }
        conversationRevisions[id] = revision
        inboxConversations.removeAll { $0.id == id }
        if let conversation, let latest {
            inboxConversations.append(MultiStandbyConversation(conversationID: conversation.id,
                profileID: conversation.profileID, peer: conversation.peer, messageCount: conversation.messageCount, latestMessage: latest))
            mergeInboxMessages([latest], revision: revision)
        }
    }

    private func refreshConversation(id: String, epoch: UInt64) async throws {
        let page = try await api.conversations(profileID: nil, conversationID: id, cursor: nil)
        guard current(epoch) else { throw CancellationError() }
        applyConversation(page.conversations.first { $0.id == id }, id: id, revision: page.revision)
    }

    /// A display-only cursor. Notification and durable-forwarding checkpoints
    /// retain their original revision-feed semantics and are never advanced here.
    func pollConversationChanges() async {
        guard isServiceRunning, let startingCursor = summaryChangesCursor, !summaryChangesInFlight else { return }
        summaryChangesInFlight = true
        defer { summaryChangesInFlight = false }
        let epoch = lifecycle
        var cursor = startingCursor
        do {
            for _ in 0..<8 {
                let page = try await api.changes(cursor: cursor)
                guard current(epoch) else { return }
                var affected = Set<String>()
                var needsLegacySnapshot = false
                for change in page.changes {
                    guard change.kind == nil || change.kind == "message" else { continue }
                    if change.direction == "outbound" { invalidateSendRecovery() }
                    let identity = change.profileID + ":" + change.messageID
                    if change.deleted == true { removeDeletedMessage(identity) }
                    if change.deleted != true, selectedProfileID == change.profileID,
                       let found = try await findMessage(change) {
                        guard current(epoch) else { return }
                        mergeMessages([found.message], revision: found.observationRevision)
                    }
                    if let id = change.conversationID { affected.insert(id) }
                    else if let existing = inboxConversations.first(where: { $0.latestMessage.id == identity }) {
                        affected.insert(existing.id)
                    } else if change.deleted != true, let found = try await findMessage(change) {
                        // Compatibility for pre-summary change records.
                        let message = found.message
                        affected.insert(SMSConversationIdentity.digest(message.profileID + "\u{0}" + message.peer))
                    } else if change.deleted == true { needsLegacySnapshot = true }
                }
                if needsLegacySnapshot {
                    await refreshSMSCenter()
                    guard current(epoch), completedInboxRequest == inboxRequest else {
                        // A legacy tombstone has no peer/key. Its display
                        // checkpoint must wait until the full reconciliation
                        // actually succeeds, rather than swallowing that error.
                        throw URLError(.cannotLoadFromNetwork)
                    }
                }
                for id in affected.sorted() { try await refreshConversation(id: id, epoch: epoch) }
                guard current(epoch) else { return }
                let next = page.nextCursor?.value ?? page.revision.map(String.init) ?? cursor
                summaryChangesCursor = next
                if page.changes.isEmpty || next == cursor { return }
                cursor = next
            }
        } catch {
            if current(epoch) { lastError = "短信会话同步失败：" + userFacingErrorMessage(error) }
        }
    }

    func control(start: Bool, profileID: String? = nil, profileIDs: [String]? = nil) async {
        await submitControl(action: start ? "start" : "stop", profileIDs: profileIDs ?? profileID.map { [$0] })
    }

    func retry(profileID: String) async {
        await submitControl(action: "retry", profileIDs: [profileID])
    }

    private func submitControl(action: String, profileIDs: [String]?) async {
        guard isServiceRunning, !controlling else { return }
        let card: String
        let selected = profileIDs.map { Array(Set($0)).sorted() }
        if let selected {
            guard !selected.isEmpty else { return }
            let known = profiles.filter { selected.contains($0.profileID) }
            guard known.count == selected.count, let first = known.first,
                  known.allSatisfy({ $0.cardID == first.cardID }) else {
                actionError = "请选择同一号卡中仍可见的线路"; return
            }
            card = first.cardID
            if action == "retry", known.contains(where: \.isStopped) {
                actionError = "请先启动所选线路"; return
            }
        } else { card = selectedCardID }
        guard !card.isEmpty else { actionError = "请先选择要操作的号卡"; return }
        if action != "stop", !cards.contains(where: { $0.cardID == card && $0.available }) {
            actionError = "号卡当前不可用，请检查连接或占用状态"; return
        }
        let intent = MultiStandbyControlIntentRequest(
            action: action, cardID: card, profileIDs: selected, all: selected == nil ? true : nil)
        controlling = true
        actionError = nil
        let epoch = lifecycle
        defer { controlling = false }
        do {
            let prepared = try await api.prepareControlIntent(intent)
            guard current(epoch) else { return }
            let request = MultiStandbyControlRequest(
                cardID: card, profileIDs: selected, all: selected == nil ? true : nil,
                clientRequestID: prepared.clientRequestID)
            let accepted: MultiStandbyAccepted
            if action == "retry" { accepted = try await api.retry(request: request) }
            else { accepted = try await api.control(start: action == "start", request: request) }
            guard current(epoch) else { return }
            operation = MultiStandbyOperation(operationID: accepted.operationID,
                state: accepted.accepted ? "pending" : "failed", detail: accepted.accepted ? "操作已提交，正在读取逐号状态" : "操作未被接受")
            // Acknowledgement only retires the durable prepare record. Failure
            // is safe: Rust retains it and a later GUI reuses the same id.
            _ = try? await api.acknowledgeControlIntent(
                MultiStandbyControlIntentAcknowledgement(
                    action: action, cardID: card, profileIDs: selected,
                    all: selected == nil ? true : nil,
                    clientRequestID: prepared.clientRequestID))
            await refreshProfiles()
        } catch {
            if current(epoch) { actionError = "操作结果待确认：" + userFacingErrorMessage(error) + "；Rust 将保留并复用原请求标识。" }
        }
    }

    func refreshRuntimeReport(hours: Int = 12) async {
        guard isServiceRunning else { return }
        reportRequest &+= 1
        let request = reportRequest
        let epoch = lifecycle
        let card = selectedCardID
        runtimeReportLoading = true
        runtimeReportError = nil
        defer { if request == reportRequest { runtimeReportLoading = false } }
        let boundedHours = min(168, max(1, hours))
        let since = Int64(Date().addingTimeInterval(-Double(boundedHours) * 3600).timeIntervalSince1970 * 1000)
        do {
            let report = try await api.runtimeReport(sinceMS: since, cardID: card)
            guard current(epoch), request == reportRequest, card == selectedCardID else { return }
            runtimeReport = report
        } catch {
            if current(epoch), request == reportRequest { runtimeReportError = userFacingErrorMessage(error) }
        }
    }

    /// A new native intent is durable before any queue call. Repeating an
    /// unresolved action queries evidence; only an explicit new-send action
    /// allocates a fresh ID after an unknown result.
    func send(phone: String, body: String, expectedProfileID: String? = nil, newIntent: Bool = false) async -> Bool {
        let phone = phone.trimmingCharacters(in: .whitespacesAndNewlines)
        let profileID = expectedProfileID ?? selectedProfileID
        guard isServiceRunning, !sending, !phone.isEmpty,
              !body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        if !sendRecoveryLoaded { await refreshSendIntents() }
        guard sendRecoveryLoaded else {
            actionError = "无法读取已有发送意图，请先查询恢复记录后再发送。"
            return false
        }
        let key = SendKey(profileID: profileID, phone: phone, body: body)
        if !newIntent, let intent = unresolvedSends.first(where: { $0.request.profileID == profileID && $0.phone == phone && $0.body == body }) {
            if intent.state == "prepared" { return await continueSendIntent(intent) }
            return await querySendIntent(intent)
        }
        let request = (!newIntent ? pendingSends[key] : nil)
            ?? MultiStandbySendRequest(profileID: profileID, phone: phone, body: body, clientRequestID: UUID().uuidString)
        return await submitSendIntent(request)
    }

    func refreshSendIntents() async {
        guard isServiceRunning else { return }
        sendRecoveryRequest &+= 1
        let request = sendRecoveryRequest
        let epoch = lifecycle
        sendRecoveryLoading = true
        defer { if request == sendRecoveryRequest { sendRecoveryLoading = false } }
        do {
            var cursor: String?
            var seen = Set<String>()
            var restored: [MultiStandbySendIntent] = []
            repeat {
                let result = try await api.sendIntents(cursor: cursor)
                guard current(epoch), request == sendRecoveryRequest else { return }
                restored.append(contentsOf: result.intents.filter(\.needsRecovery))
                cursor = result.nextCursor?.value
                if let cursor, !seen.insert(cursor).inserted { throw CocoaError(.fileReadCorruptFile) }
            } while cursor != nil
            unresolvedSends = restored.sorted { $0.createdAtMS > $1.createdAtMS }
            sendRecoveryLoaded = true
        } catch {
            if current(epoch), request == sendRecoveryRequest {
                actionError = "发送恢复记录暂不可读取，请稍后查询：" + userFacingErrorMessage(error)
            }
        }
    }

    @discardableResult
    func querySendIntent(_ intent: MultiStandbySendIntent) async -> Bool {
        guard isServiceRunning, !sending else { return false }
        let epoch = lifecycle
        let recoveryRequest = sendRecoveryRequest
        do {
            let result = try await api.sendIntent(profileID: intent.profileID, clientRequestID: intent.clientRequestID)
            guard current(epoch), recoveryRequest == sendRecoveryRequest,
                  result.profileID == intent.profileID, result.clientRequestID == intent.clientRequestID else { return false }
            recordSendIntent(result)
            let accepted = Self.hasQueueEvidence(result.state)
            sendNotice = "原发送请求 · " + Self.messageState(result.state)
            actionError = result.state == "unknown" ? "上次提交结果待确认，未自动重发。若要再发，请明确选择新发送，可能产生重复短信。" : nil
            if selectedProfileID == intent.profileID { await refreshMessages() }
            return accepted
        } catch {
            if current(epoch) { actionError = "原发送结果暂不可确认，未重新提交：" + userFacingErrorMessage(error) }
            return false
        }
    }

    func continueSendIntent(_ intent: MultiStandbySendIntent) async -> Bool {
        guard intent.state == "prepared" else { return await querySendIntent(intent) }
        return await submitSendIntent(intent.request)
    }

    private func submitSendIntent(_ request: MultiStandbySendRequest) async -> Bool {
        let profile = profiles.first { $0.profileID == request.profileID }
            ?? inboxProfiles.first { $0.profileID == request.profileID }
        guard isServiceRunning, !sending, profile?.isReady == true else {
            actionError = "原发送线路尚未就绪，未切换线路或提交短信。"
            return false
        }
        let key = SendKey(profileID: request.profileID, phone: request.phone, body: request.body)
        pendingSends[key] = request
        sending = true
        actionError = nil
        sendNotice = nil
        let epoch = lifecycle
        var preparationConfirmed = false
        defer { sending = false }
        do {
            let prepared = try await api.prepareSend(request)
            guard current(epoch), prepared.request == request else { return false }
            preparationConfirmed = true
            recordSendIntent(prepared)
            if prepared.state != "prepared" {
                actionError = "原发送意图已进入提交阶段，仅查询结果；未再次提交。"
                return Self.hasQueueEvidence(prepared.state)
            }
            let authorized = try await api.authorizeSend(request)
            guard current(epoch), authorized.request == request else { return false }
            recordSendIntent(authorized)
            guard authorized.canSubmit else {
                actionError = "原发送意图已进入提交阶段，未再次提交。请查询结果，或明确新发送。"
                return Self.hasQueueEvidence(authorized.state)
            }
            let result = try await api.send(request)
            guard current(epoch) else { return false }
            pendingSends.removeValue(forKey: key)
            unresolvedSends.removeAll { $0.id == authorized.id }
            sendRecoveryRequest &+= 1
            sendNotice = "已保存发送请求 · " + Self.messageState(result.state)
            if selectedProfileID == request.profileID { await refreshMessages() }
            return true
        } catch {
            guard current(epoch) else { return false }
            // This read can discover a committed queue whose response was lost.
            // Failure to query never causes a write replay.
            if let result = try? await api.sendIntent(profileID: request.profileID, clientRequestID: request.clientRequestID), current(epoch) {
                preparationConfirmed = true
                recordSendIntent(result)
                if Self.hasQueueEvidence(result.state) {
                    pendingSends.removeValue(forKey: key)
                    sendNotice = "已查询原发送请求 · " + Self.messageState(result.state)
                    if selectedProfileID == request.profileID { await refreshMessages() }
                    return true
                }
                if ["failed", "cancelled"].contains(result.state) {
                    sendNotice = "已查询原发送请求 · " + Self.messageState(result.state)
                    actionError = "原发送请求已结束；再次发送会创建独立的新请求。"
                    return false
                }
            }
            actionError = preparationConfirmed
                ? "发送意图已保存，请查询或恢复原请求；结果待确认时不会自动重发。"
                : "发送意图尚未确认保存，未调用发送接口；请先查询恢复记录后重试。"
            return false
        }
    }

    private func recordSendIntent(_ intent: MultiStandbySendIntent) {
        sendRecoveryRequest &+= 1
        sendRecoveryLoading = false
        unresolvedSends.removeAll { $0.id == intent.id }
        if intent.needsRecovery { unresolvedSends.append(intent) }
        else {
            pendingSends = pendingSends.filter { _, request in
                request.profileID != intent.profileID || request.clientRequestID != intent.clientRequestID
            }
        }
    }

    private func invalidateSendRecovery() {
        sendRecoveryRequest &+= 1
        sendRecoveryLoaded = false
        sendRecoveryLoading = false
    }

    private static func hasQueueEvidence(_ state: String) -> Bool {
        ["queued", "submitting", "accepted", "delivered"].contains(state)
    }

    func delete(_ message: MultiStandbyMessage) async {
        guard isServiceRunning, message.profileID == selectedProfileID else { return }
        let profile = message.profileID
        let epoch = lifecycle
        deleteRequest &+= 1
        let request = deleteRequest
        activeDeleteRequests[message.id] = request
        do {
            try await api.delete(messageID: message.messageID, profileID: profile)
            guard current(epoch), activeDeleteRequests[message.id] == request else { return }
            activeDeleteRequests.removeValue(forKey: message.id)
            actionError = nil
            removeDeletedMessage(message.id)
            do {
                try await refreshConversation(id: SMSConversationIdentity.digest(profile + "\u{0}" + message.peer), epoch: epoch)
            } catch {
                if current(epoch) { lastError = "短信已删除，会话同步稍后重试：" + userFacingErrorMessage(error) }
                return
            }
            if selectedProfileID == profile { await refreshMessages() }
        } catch {
            guard current(epoch), activeDeleteRequests[message.id] == request else { return }
            activeDeleteRequests.removeValue(forKey: message.id)
            actionError = userFacingErrorMessage(error)
        }
    }

    private func removeDeletedMessage(_ identity: String) {
        deletedMessageIDs.insert(identity)
        evictCachedMessage(identity)
        // History may have taken its snapshot before this deletion. Its body
        // cannot reappear, regardless of completion order or cursor position.
        messages.removeAll { $0.id == identity }
        inboxMessages.removeAll { $0.id == identity }
        // Counts and the next preview require an authoritative snapshot. Hide
        // a summary whose latest body was deleted until reconciliation succeeds.
        inboxConversations.removeAll { $0.latestMessage.id == identity }
        attentionStore.forgetSMS(messageID: identity)
    }

    func pollChanges() async {
        guard isServiceRunning, !changesInFlight else { return }
        changesInFlight = true
        defer { changesInFlight = false }
        let epoch = lifecycle
        do {
            // Bounded batches keep a large archive from starving status polls.
            for _ in 0..<8 {
                let previous = changesCursor
                let page = try await api.changes(cursor: previous)
                guard current(epoch) else { return }
                if page.changes.contains(where: { $0.direction == "outbound" }) { invalidateSendRecovery() }
                if notificationsSeeded {
                    for change in page.changes {
                        let identity = change.profileID + ":" + change.messageID
                        if change.deleted == true {
                            removeDeletedMessage(identity)
                            continue
                        }
                        guard change.kind == nil || change.kind == "message" else { continue }
                        let shouldNotify = !notifiedIDs.contains(identity)
                            && !["out", "outbound"].contains(change.direction ?? "")
                            && (change.state == nil || change.state == "received")
                        let shouldIndexInbox = viewingInbox && inboxProfiles.contains {
                            $0.profileID == change.profileID
                        }
                        guard shouldNotify || selectedProfileID == change.profileID || shouldIndexInbox else { continue }
                        guard let found = try await findMessage(change), current(epoch) else { continue }
                        let message = found.message
                        if selectedProfileID == message.profileID { mergeMessages([message], revision: found.observationRevision) }
                        if shouldIndexInbox { mergeInboxMessages([message], revision: found.observationRevision) }
                        guard shouldNotify && message.isIncoming else { continue }
                        let threadID = SMSConversationIdentity.multiStandby(profileID: message.profileID, peer: message.peer)
                        attentionStore.recordIncomingSMS(messageID: message.id, threadID: threadID)
                        let visible = attentionStore.isViewingSMS(threadID: threadID)
                        if notificationsEnabled && !visible {
                            let masked = defaults.object(forKey: AppPreferenceKeys.privacyMaskEnabled) as? Bool ?? true
                            try await notificationSink(message, masked)
                            guard current(epoch) else { return }
                        }
                        notifiedIDs.append(identity)
                        notifiedIDs = Array(notifiedIDs.suffix(2048))
                        defaults.set(notifiedIDs, forKey: Self.notifiedKey)
                    }
                }
                guard current(epoch) else { return }
                if let next = page.nextCursor?.value, !next.isEmpty { changesCursor = next }
                defaults.set(changesCursor, forKey: Self.cursorKey)
                if page.changes.isEmpty || changesCursor == previous {
                    notificationsSeeded = true
                    defaults.set(true, forKey: Self.seededKey)
                    return
                }
            }
        } catch {
            if current(epoch) { lastError = "多待短信同步：" + userFacingErrorMessage(error) }
        }
    }

    private func findMessage(_ change: MultiStandbyChange) async throws -> (message: MultiStandbyMessage, observationRevision: UInt64)? {
        // A change revision is a durable archive position, so a new arrival
        // does not require scanning years of earlier messages. Historical
        // notification clicks have no revision and follow pages to exhaustion.
        var cursor: String? = change.revision > 0 ? String(change.revision - 1) : nil
        let epoch = lifecycle
        while current(epoch) {
            let page = try await api.messages(profileID: change.profileID, cursor: cursor)
            guard current(epoch) else { throw CancellationError() }
            if let message = page.messages.first(where: { $0.profileID == change.profileID && $0.messageID == change.messageID }) {
                // Preserve the wire's optional per-message revision. The page
                // position supplies the existing resolver fallback only when
                // that field is absent, including old notification deep links.
                return (message, page.revision)
            }
            guard let next = page.nextCursor?.value, !next.isEmpty, next != cursor else { return nil }
            cursor = next
        }
        throw CancellationError()
    }

    private func current(_ epoch: UInt64) -> Bool { isServiceRunning && lifecycle == epoch && !Task.isCancelled }

    private func resolveMessages(_ incoming: [MultiStandbyMessage], revision: UInt64?) -> [MultiStandbyMessage] {
        incoming.compactMap { message in
            guard !deletedMessageIDs.contains(message.id) else { return nil }
            let version = message.revision ?? revision ?? 0
            if (messageRevisionFloors[message.id] ?? 0) > version {
                guard let cached = messageCache[message.id],
                      cached.revision >= (messageRevisionFloors[message.id] ?? 0) else { return nil }
                touchCachedMessage(message.id)
                return cached.message
            }
            messageRevisionFloors[message.id] = version
            cacheMessageBody(message, revision: version)
            return message
        }
    }

    private func touchCachedMessage(_ id: String) {
        messageCacheOrder.removeAll { $0 == id }
        messageCacheOrder.append(id)
    }

    private func evictCachedMessage(_ id: String) {
        if let previous = messageCache.removeValue(forKey: id) {
            messageCacheBodyBytes -= previous.bodyBytes
        }
        messageCacheOrder.removeAll { $0 == id }
    }

    private func auxiliaryBodyPriority(_ message: MultiStandbyMessage) -> Int {
        guard message.profileID == selectedProfileID else { return 0 }
        if message.messageID == selectedMessageID { return 2 }
        return selectedPeer == nil || message.peer == selectedPeer ? 1 : 0
    }

    private func cacheMessageBody(_ message: MultiStandbyMessage, revision: UInt64) {
        let bytes = message.body.utf8.count
        evictCachedMessage(message.id)
        // A single oversized message remains displayable and addressable via
        // the archive API; pinning a target can never bypass the cache budget.
        guard bytes <= Self.maximumCachedBodyBytes else { return }
        messageCache[message.id] = CachedMessage(message: message, revision: revision, bodyBytes: bytes)
        messageCacheBodyBytes += bytes
        messageCacheOrder.append(message.id)
        while messageCache.count > Self.maximumCachedMessages || messageCacheBodyBytes > Self.maximumCachedBodyBytes {
            let victim = messageCacheOrder.first { id in
                messageCache[id].map { auxiliaryBodyPriority($0.message) == 0 } ?? true
            } ?? messageCacheOrder.first { id in
                messageCache[id].map { auxiliaryBodyPriority($0.message) < 2 } ?? true
            } ?? messageCacheOrder.first!
            evictCachedMessage(victim)
        }
    }

    private func trimInboxMessageBodies(_ candidates: [MultiStandbyMessage]) -> [MultiStandbyMessage] {
        let prioritized = candidates.sorted { a, b in
            let left = auxiliaryBodyPriority(a), right = auxiliaryBodyPriority(b)
            if left != right { return left > right }
            if a.createdAtMS != b.createdAtMS { return a.createdAtMS > b.createdAtMS }
            return a.id > b.id
        }
        var retained: [MultiStandbyMessage] = []
        var bytes = 0
        for message in prioritized {
            let size = message.body.utf8.count
            guard retained.count < Self.maximumInboxMessages,
                  size <= Self.maximumInboxBodyBytes - bytes else { continue }
            retained.append(message)
            bytes += size
        }
        return retained.sorted {
            $0.createdAtMS == $1.createdAtMS ? $0.messageID < $1.messageID : $0.createdAtMS < $1.createdAtMS
        }
    }

    private func mergeMessages(_ incoming: [MultiStandbyMessage], revision: UInt64? = nil) {
        var unique: [String: MultiStandbyMessage] = [:]
        for message in messages + resolveMessages(incoming, revision: revision) where message.profileID == selectedProfileID
            && (selectedPeer == nil || message.peer == selectedPeer) && !deletedMessageIDs.contains(message.id) {
            unique[message.id] = message
        }
        messages = unique.values.sorted {
            $0.createdAtMS == $1.createdAtMS ? $0.messageID < $1.messageID : $0.createdAtMS < $1.createdAtMS
        }
        if viewingInbox { mergeInboxMessages(incoming, revision: revision) }
    }

    private func mergeInboxMessages(_ incoming: [MultiStandbyMessage], revision: UInt64? = nil) {
        var unique = Dictionary(inboxMessages.map { ($0.id, $0) }, uniquingKeysWith: { _, newest in newest })
        for message in resolveMessages(incoming, revision: revision) { unique[message.id] = message }
        inboxMessages = trimInboxMessageBodies(Array(unique.values))
    }

    private static func postNotification(_ message: MultiStandbyMessage, masked: Bool) async throws {
        guard !AcceptanceBootstrap.isAcceptance else { return }
        let center = UNUserNotificationCenter.current()
        var settings = await center.notificationSettings()
        if settings.authorizationStatus == .notDetermined {
            guard try await center.requestAuthorization(options: [.alert, .sound]) else { return }
            settings = await center.notificationSettings()
        }
        guard settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional else { return }
        let content = UNMutableNotificationContent()
        content.title = "N待 · 新短信"
        content.body = masked ? "内容已隐藏，请打开 Switch 查看" : message.peer + "：" + message.body
        content.sound = .default
        content.userInfo = ["source": "multistandby", "profile_id": message.profileID, "message_id": message.messageID]
        try await center.add(UNNotificationRequest(identifier: "switch.multistandby." + message.id, content: content, trigger: nil))
    }

    static func messageState(_ state: String) -> String {
        switch state {
        case "prepared": return "已保存，尚未提交"
        case "received": return "已接收"
        case "queued": return "等待发送"
        case "submitting": return "正在提交"
        case "accepted": return "运营商已接受"
        case "delivered": return "已送达"
        case "failed": return "发送失败"
        case "unknown": return "结果待确认"
        case "cancelled": return "已取消"
        default: return state
        }
    }
}
