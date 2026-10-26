import Foundation
import Combine

enum DeviceSMSSendState: String, Decodable { case reserved, prepared, unknown, accepted, cancelled }

struct DeviceSMSIntentRequest: Codable, Equatable {
    let clientRequestID: String
    let phone: String
    let message: String
    let device: String
    let expectedSubscriptionID: String?
    var identityVersion: Int? = nil
    enum CodingKeys: String, CodingKey {
        case phone, message, device
        case clientRequestID = "client_request_id"
        case expectedSubscriptionID = "expected_subscription_id"
        case identityVersion = "identity_version"
    }
}

struct DeviceSMSIntent: Decodable, Identifiable, Equatable {
    let clientRequestID: String
    let phone: String
    let message: String
    let device: String
    let subscriptionID: String?
    let state: DeviceSMSSendState
    let segments: Int
    let archiveConfirmed: Bool
    let canSubmit: Bool
    let createdAtMS: Int64
    let updatedAtMS: Int64
    var identityVersion: Int? = nil
    var intentNonce: String? = nil
    var expectedSubscriptionID: String? = nil
    var version: Int { identityVersion ?? 1 }
    var id: String { clientRequestID }
    var needsRecovery: Bool { state != .cancelled && (state != .accepted || !archiveConfirmed) }
    var request: DeviceSMSIntentRequest {
        DeviceSMSIntentRequest(clientRequestID: clientRequestID, phone: phone, message: message,
                               device: device, expectedSubscriptionID: expectedSubscriptionID ?? subscriptionID, identityVersion: identityVersion)
    }
    var statusText: String {
        switch state {
        case .reserved: "签发候选已保存，尚未提交"
        case .cancelled: "已取消，未提交"
        case .prepared: canSubmit ? "已保存，尚未提交" : "原线路已失效，尚未提交"
        case .unknown: "提交结果待确认"
        case .accepted: archiveConfirmed ? "通道已接受，未确认送达" : "通道已接受，本机历史未确认"
        }
    }
    func matches(_ request: DeviceSMSIntentRequest) -> Bool {
        version == (request.identityVersion ?? 1) && clientRequestID == request.clientRequestID && phone == request.phone && message == request.message
            && device == request.device
            && (request.expectedSubscriptionID == nil || request.expectedSubscriptionID == "" || subscriptionID == request.expectedSubscriptionID)
    }
    enum CodingKeys: String, CodingKey {
        case phone, message, device, state, segments
        case clientRequestID = "client_request_id", subscriptionID = "subscription_id"
        case archiveConfirmed = "archive_confirmed", canSubmit = "can_submit"
        case createdAtMS = "created_at_ms", updatedAtMS = "updated_at_ms"
        case identityVersion = "identity_version", intentNonce = "intent_nonce"
        case expectedSubscriptionID = "expected_subscription_id"
    }
}

struct DeviceSMSIntentPage: Decodable {
    let intents: [DeviceSMSIntent]
    let nextCursor: String?
    enum CodingKeys: String, CodingKey { case intents; case nextCursor = "next_cursor" }
}

@MainActor protocol DeviceSMSSendAPI {
    func list(cursor: String?) async throws -> DeviceSMSIntentPage
    func get(id: String) async throws -> DeviceSMSIntent
    func prepare(_ request: DeviceSMSIntentRequest) async throws -> DeviceSMSIntent
    func submit(_ request: DeviceSMSIntentRequest) async throws
    func reconcileArchive(_ request: DeviceSMSIntentRequest) async throws
    func authority() async throws -> DeviceSMSAuthorityStatus
    func reserve(bytes: Data) async throws -> DeviceSMSIntent
    func prepareV2(bytes: Data) async throws -> DeviceSMSIntent
    func submitV2(bytes: Data) async throws
    func reconcileArchiveV2(bytes: Data) async throws
    func cancelReservation(bytes: Data) async throws -> DeviceSMSIntent
}

struct DeviceSMSPendingError: LocalizedError {
    let reason: String
    var errorDescription: String? { "结果待确认：" + reason }
}

/// Persistent intent ownership lives in the backend. Memory holds only the
/// current UI's pending ID; restart recovery queries the durable intent list.
@MainActor final class DeviceSMSSendStore: ObservableObject {
    @Published private(set) var intents: [DeviceSMSIntent] = []
    @Published private(set) var sending = false
    @Published private(set) var loading = false
    @Published private(set) var available = false
    @Published private(set) var error: String?
    @Published private(set) var notice: String?
    private let api: any DeviceSMSSendAPI
    private var epoch: UInt64 = 0
    private var queryRevision: UInt64 = 0
    private var loaded = false
    private var recoveryRecords: [DeviceSMSIntent] = []
    private let preferences: UserDefaults?
    private var dismissed: Set<String>
    private static let dismissedKey = "SwitchDismissedDeviceSendIntents"
    private var pending: [Payload: DeviceSMSIntentRequest] = [:]
    private struct Payload: Hashable {
        let phone: String, message: String, device: String
        let subscription: String?
        init(_ request: DeviceSMSIntentRequest) {
            phone = request.phone; message = request.message; device = request.device
            subscription = request.expectedSubscriptionID
        }
    }

    init(api: any DeviceSMSSendAPI, preferences: UserDefaults? = nil) {
        self.api = api
        self.preferences = preferences
        self.dismissed = Set(preferences?.stringArray(forKey: Self.dismissedKey) ?? [])
    }

    // Clear the recovery panel, retaining the backend's at-most-once ledger.
    // An uncertain request must never regain submission permission by deletion.
    func dismiss(_ intent: DeviceSMSIntent) {
        guard !sending else { return }
        dismissed.insert(intent.id)
        publishRecoveryRecords()
    }

    func clearRecoveryRecords() {
        guard !sending else { return }
        dismissed.formUnion(intents.map(\.id))
        error = nil
        publishRecoveryRecords()
    }

    private func publishRecoveryRecords() {
        preferences?.set(dismissed.sorted(), forKey: Self.dismissedKey)
        intents = recoveryRecords.filter { !dismissed.contains($0.id) }
            .sorted { $0.createdAtMS > $1.createdAtMS }
    }

    func setAvailable(_ value: Bool) {
        guard value != available else { return }
        available = value
        epoch &+= 1
        queryRevision &+= 1
        loading = false
        loaded = false
        sending = false
    }

    private func current(_ generation: UInt64) -> Bool {
        available && generation == epoch && !Task.isCancelled
    }

    func refresh() async {
        guard available else { return }
        let generation = epoch
        queryRevision &+= 1
        let query = queryRevision
        loading = true
        defer { if query == queryRevision { loading = false } }
        do {
            var cursor: String?
            var seen = Set<String>()
            var rows: [DeviceSMSIntent] = []
            repeat {
                let page = try await api.list(cursor: cursor)
                guard current(generation), query == queryRevision else { return }
                guard rows.count + page.intents.count <= 10_000 else { throw CocoaError(.fileReadCorruptFile) }
                rows.append(contentsOf: page.intents)
                cursor = page.nextCursor
                if let cursor, cursor.isEmpty || !seen.insert(cursor).inserted { throw CocoaError(.fileReadCorruptFile) }
            } while cursor != nil
            guard Set(rows.map(\.id)).count == rows.count else { throw CocoaError(.fileReadCorruptFile) }
            recoveryRecords = rows.filter(\.needsRecovery)
            publishRecoveryRecords()
            loaded = true
            error = nil
        } catch {
            if current(generation), query == queryRevision { self.error = "发送恢复记录暂不可读取，请稍后查询。" }
        }
    }

    @discardableResult
    func query(_ intent: DeviceSMSIntent) async -> DeviceSMSIntent? {
        guard available, !sending else { return nil }
        let generation = epoch
        queryRevision &+= 1
        let query = queryRevision
        do {
            let record = try await api.get(id: intent.id)
            guard current(generation), query == queryRevision, record.matches(intent.request) else { return nil }
            recordResult(record)
            notice = record.statusText
            error = nil
            return record
        } catch {
            if current(generation), query == queryRevision { self.error = "原请求暂不可确认，未重新提交。" }
            return nil
        }
    }

    func send(phone: String, message: String, device: String, subscriptionID: String?, newRequest: Bool = false) async throws -> DeviceSMSIntent {
        guard available, !sending else { throw DeviceSMSPendingError(reason: "本机服务尚未就绪或已有发送操作。") }
        if !loaded { await refresh() }
        guard available, loaded else { throw DeviceSMSPendingError(reason: "先读取已有发送记录，再尝试发送。") }
        let fresh = DeviceSMSIntentRequest(clientRequestID: UUID().uuidString, phone: phone, message: message,
                                           device: device, expectedSubscriptionID: subscriptionID)
        let key = Payload(fresh)
        if !newRequest, let previous = recoveryRecords.first(where: {
            $0.phone == phone && $0.message == message && $0.device == device
                && (subscriptionID == nil || $0.subscriptionID == subscriptionID)
        }) {
            if let result = await query(previous), result.state == .accepted && result.archiveConfirmed { return result }
            throw DeviceSMSPendingError(reason: "保留了原发送请求；请核对原请求，或使用重新发送按钮，可能产生重复短信。")
        }
        return try await submit(newRequest ? fresh : (pending[key] ?? fresh))
    }

    func resume(_ intent: DeviceSMSIntent) async throws -> DeviceSMSIntent {
        if intent.state == .accepted && !intent.archiveConfirmed {
            return try await reconcileArchive(intent)
        }
        guard intent.state == .prepared && intent.canSubmit else {
            if let record = await query(intent), record.state == .accepted && record.archiveConfirmed { return record }
            throw DeviceSMSPendingError(reason: "原请求只能查询，未再次提交。")
        }
        return try await submit(intent.request)
    }

    /// An explicit recovery action keeps the original identity and payload.
    /// It never prepares another request, chooses a new line, or retries send.
    private func reconcileArchive(_ intent: DeviceSMSIntent) async throws -> DeviceSMSIntent {
        guard available, !sending else { throw DeviceSMSPendingError(reason: "本机服务尚未就绪或已有发送操作。") }
        let generation = epoch
        sending = true
        error = nil
        defer { if generation == epoch { sending = false } }
        do {
            let original = try await api.get(id: intent.id)
            guard current(generation) else { throw CancellationError() }
            guard original.matches(intent.request), original.state == .accepted else {
                throw DeviceSMSPendingError(reason: "原请求状态不符，未补归档。")
            }
            if !original.archiveConfirmed {
                try await api.reconcileArchive(original.request)
            }
            guard current(generation) else { throw CancellationError() }
            let result = try await api.get(id: intent.id)
            guard current(generation) else { throw CancellationError() }
            guard result.matches(intent.request) else { throw CocoaError(.fileReadCorruptFile) }
            recordResult(result)
            guard result.state == .accepted && result.archiveConfirmed else {
                throw DeviceSMSPendingError(reason: "本机归档仍待确认，原请求已保留。")
            }
            notice = "本机归档已确认；未再次发送短信。"
            return result
        } catch {
            guard current(generation) else { throw CancellationError() }
            if let result = try? await api.get(id: intent.id),
               current(generation), result.matches(intent.request),
               result.state == .accepted && result.archiveConfirmed {
                recordResult(result)
                notice = "本机归档已确认；未再次发送短信。"
                return result
            }
            guard current(generation) else { throw CancellationError() }
            self.error = "补归档尚未确认，保留原请求；未再次发送短信。"
            throw DeviceSMSPendingError(reason: self.error!)
        }
    }

    private func submit(_ request: DeviceSMSIntentRequest) async throws -> DeviceSMSIntent {
        guard available, !sending else { throw DeviceSMSPendingError(reason: "本机服务尚未就绪或已有发送操作。") }
        let generation = epoch
        pending[Payload(request)] = request
        sending = true
        error = nil
        notice = nil
        defer { if generation == epoch { sending = false } }
        do {
            let prepared = try await api.prepare(request)
            guard current(generation) else { throw CancellationError() }
            guard prepared.matches(request) else { throw CocoaError(.fileReadCorruptFile) }
            recordResult(prepared)
            if prepared.state == .accepted && prepared.archiveConfirmed { return prepared }
            guard prepared.state == .prepared && prepared.canSubmit else {
                throw DeviceSMSPendingError(reason: "原请求已提交或原线路已失效；未再次提交。")
            }
            try await api.submit(prepared.request)
            guard current(generation) else { throw CancellationError() }
            let result = try await api.get(id: request.clientRequestID)
            guard current(generation) else { throw CancellationError() }
            guard result.matches(request) else { throw CocoaError(.fileReadCorruptFile) }
            recordResult(result)
            notice = result.statusText
            guard result.state == .accepted && result.archiveConfirmed else {
                throw DeviceSMSPendingError(reason: result.statusText + "，请先核对，勿重复提交。")
            }
            return result
        } catch {
            guard current(generation) else { throw CancellationError() }
            // Reading after a failed HTTP response may discover an accepted
            // submission. It never grants another POST or a new request ID.
            if let record = try? await api.get(id: request.clientRequestID), current(generation), record.matches(request) {
                recordResult(record)
                notice = record.statusText
                if record.state == .accepted && record.archiveConfirmed { return record }
            }
            guard current(generation) else { throw CancellationError() }
            let message = "原发送请求已保留；请查询或恢复。结果待确认时不会自动重发。"
            self.error = message
            throw DeviceSMSPendingError(reason: message)
        }
    }

    private func recordResult(_ record: DeviceSMSIntent) {
        queryRevision &+= 1
        loading = false
        recoveryRecords.removeAll { $0.id == record.id }
        if record.needsRecovery { recoveryRecords.append(record) }
        else { pending = pending.filter { $0.value.clientRequestID != record.id } }
        publishRecoveryRecords()
    }
}
