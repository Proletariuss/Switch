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
    var intentNonce: String? = nil
    enum CodingKeys: String, CodingKey {
        case phone, message, device
        case clientRequestID = "client_request_id"
        case expectedSubscriptionID = "expected_subscription_id"
        case identityVersion = "identity_version", intentNonce = "intent_nonce"
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
                               device: device, expectedSubscriptionID: expectedSubscriptionID ?? subscriptionID, identityVersion: identityVersion, intentNonce: intentNonce)
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
            && (request.intentNonce == nil || intentNonce == request.intentNonce)
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

struct DeviceSMSIdentityRefusal: Error {}

struct DeviceSMSPendingError: LocalizedError {
    let reason: String
    var errorDescription: String? { "结果待确认：" + reason }
}

/// Rust owns issuance and dispatch. This GUI retains exact pre-network bytes
/// durably so a lost reserve ACK or process restart never invents a new ID.
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
    private let journal: DeviceSMSIssuanceJournal?
    private var issuancePhases: [String: DeviceSMSIssuanceCheckpoint.Phase] = [:]

    init(api: any DeviceSMSSendAPI, preferences: UserDefaults? = nil, journal: DeviceSMSIssuanceJournal? = nil) {
        self.api = api
        self.journal = journal
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
            _ = try journal?.entries() // Fail closed before trusting a fresh remote list.
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
            for row in rows { try journal?.confirmTerminal(row) }
            let checkpoints = try journal?.entries() ?? []
            for checkpoint in checkpoints {
                var projection = try checkpoint.projection()
                if let index = rows.firstIndex(where: { $0.id == projection.id }) {
                    if try !rows[index].matches(checkpoint.request) {
                        let unresolved = try journal?.advance(checkpoint, to: .unresolved)
                        projection = try unresolved?.projection() ?? projection
                        rows[index] = projection
                    }
                } else { rows.append(projection) }
            }
            issuancePhases = try Dictionary(uniqueKeysWithValues: (journal?.entries() ?? []).map { (try $0.request.clientRequestID, $0.phase) })
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
            try recordResult(record)
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
        guard available, loaded, !sending, let journal else { throw DeviceSMSPendingError(reason: "先确认本机发送日志和已有记录，再尝试发送。") }
        let phone = phone.trimmingCharacters(in: .whitespacesAndNewlines)
        let entries: [DeviceSMSIssuanceCheckpoint]
        do { entries = try journal.entries() }
        catch { throw DeviceSMSPendingError(reason: "本机发送日志不可读取，未创建新请求。") }
        let existing = try entries.first { try $0.samePayload(phone: phone, message: message, device: device, subscription: subscriptionID) }
        if !newRequest, existing == nil, let previous = recoveryRecords.first(where: {
            $0.phone == phone && $0.message == message && $0.device == device
                && (subscriptionID == nil || subscriptionID == "" || $0.subscriptionID == subscriptionID)
        }) {
            if let result = await query(previous), result.state == .accepted && result.archiveConfirmed { return result }
            throw DeviceSMSPendingError(reason: "保留原发送请求；旧请求只能查询，明确的新短信须另行确认。")
        }
        let generation = epoch
        sending = true
        error = nil
        notice = nil
        defer { if generation == epoch { sending = false } }
        do {
            let checkpoint: DeviceSMSIssuanceCheckpoint
            if !newRequest, let existing { checkpoint = existing }
            else {
                let status = try await api.authority()
                guard current(generation) else { throw CancellationError() }
                let reservation = try DeviceSMSReserveRequest.candidate(status: status, phone: phone,
                    message: message, device: device, subscription: subscriptionID)
                // insert rechecks concurrent same-payload/candidate ownership under flock.
                checkpoint = try journal.insert(.make(reservation), explicitNew: newRequest)
            }
            return try await continueIssuance(checkpoint, allowPrepared: false, generation: generation)
        } catch is CancellationError { throw CancellationError() }
        catch {
            guard current(generation) else { throw CancellationError() }
            self.error = "原候选和业务内容已保留；未自动创建其他请求。可查询原请求，或明确确认一条新短信。"
            throw DeviceSMSPendingError(reason: self.error!)
        }
    }

    func resume(_ intent: DeviceSMSIntent) async throws -> DeviceSMSIntent {
        if intent.state == .accepted && !intent.archiveConfirmed { return try await reconcileArchive(intent) }
        guard intent.version == 2, available, !sending, let journal,
              let checkpoint = try journal.checkpoint(id: intent.id),
              try intent.matches(checkpoint.request),
              intent.state == .reserved || (intent.state == .prepared && intent.canSubmit)
        else {
            if let record = await query(intent), record.state == .accepted && record.archiveConfirmed { return record }
            throw DeviceSMSPendingError(reason: "原请求只能查询，未重建签发日志或再次提交。")
        }
        let generation = epoch
        sending = true
        defer { if generation == epoch { sending = false } }
        return try await continueIssuance(checkpoint, allowPrepared: true, generation: generation)
    }

    func canResume(_ intent: DeviceSMSIntent) -> Bool {
        guard intent.version == 2, intent.state == .reserved || (intent.state == .prepared && intent.canSubmit),
              let phase = issuancePhases[intent.id] else { return false }
        return [.candidate, .reserved, .prepared].contains(phase)
    }

    /// Explicit cancellation only. Missing local journals can reconstruct this
    /// nonce-bound cancellation DTO, never an issuance or physical-send DTO.
    func cancelReservation(_ intent: DeviceSMSIntent) async throws -> DeviceSMSIntent {
        guard available, !sending, intent.version == 2, intent.state == .reserved else {
            throw DeviceSMSPendingError(reason: "只能取消明确尚未提交的签发记录。")
        }
        let generation = epoch
        sending = true
        defer { if generation == epoch { sending = false } }
        do {
            let currentRecord = try await api.get(id: intent.id)
            guard current(generation) else { throw CancellationError() }
            guard currentRecord.state == .reserved, currentRecord.matches(intent.request) else { throw DeviceSMSIdentityRefusal() }
            let reservation: DeviceSMSReserveRequest
            let bytes: Data
            if let checkpoint = try journal?.checkpoint(id: intent.id) {
                reservation = try checkpoint.reserve
                guard [.candidate, .reserved].contains(checkpoint.phase),
                      currentRecord.intentNonce == reservation.intentNonce else { throw DeviceSMSIdentityRefusal() }
                bytes = checkpoint.reserveBytes
            } else {
                reservation = try DeviceSMSReserveRequest.cancellation(currentRecord)
                bytes = try JSONEncoder().encode(reservation)
            }
            let result = try await api.cancelReservation(bytes: bytes)
            guard current(generation) else { throw CancellationError() }
            guard result.state == .cancelled, result.matches(reservation.request),
                  result.intentNonce == reservation.intentNonce else { throw DeviceSMSIdentityRefusal() }
            try recordResult(result)
            notice = "已取消原未提交记录；没有发送短信。"
            error = nil
            return result
        } catch {
            guard current(generation) else { throw CancellationError() }
            self.error = "取消结果尚未确认；原记录保留，未发送或创建新请求。"
            throw DeviceSMSPendingError(reason: self.error!)
        }
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
                if let checkpoint = try journal?.checkpoint(id: original.id) {
                    guard try original.matches(checkpoint.request) else { throw DeviceSMSIdentityRefusal() }
                    try await api.reconcileArchiveV2(bytes: checkpoint.sendBytes)
                } else {
                    try await api.reconcileArchive(original.request)
                }
            }
            guard current(generation) else { throw CancellationError() }
            let result = try await api.get(id: intent.id)
            guard current(generation) else { throw CancellationError() }
            guard result.matches(intent.request) else { throw CocoaError(.fileReadCorruptFile) }
            try recordResult(result)
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
                try recordResult(result)
                notice = "本机归档已确认；未再次发送短信。"
                return result
            }
            guard current(generation) else { throw CancellationError() }
            self.error = "补归档尚未确认，保留原请求；未再次发送短信。"
            throw DeviceSMSPendingError(reason: self.error!)
        }
    }

    private func continueIssuance(_ saved: DeviceSMSIssuanceCheckpoint, allowPrepared: Bool, generation: UInt64) async throws -> DeviceSMSIntent {
        guard let journal else { throw DeviceSMSJournalError.unavailable }
        var checkpoint = saved
        let request = try checkpoint.request
        do {
            // These states never earn a POST merely because the user reopened UI.
            if [.dispatched, .unresolved].contains(checkpoint.phase) || (checkpoint.phase == .prepared && !allowPrepared) {
                let result = try await api.get(id: request.clientRequestID)
                guard current(generation), result.matches(request) else { throw DeviceSMSIdentityRefusal() }
                try recordResult(result)
                if result.state == .accepted && result.archiveConfirmed { return result }
                throw DeviceSMSPendingError(reason: "原请求仅查询，未再次提交。")
            }
            if checkpoint.phase == .candidate {
                let reserved = try await api.reserve(bytes: checkpoint.reserveBytes)
                guard current(generation) else { throw CancellationError() }
                guard reserved.matches(request) else { throw DeviceSMSIdentityRefusal() }
                if reserved.state == .accepted || reserved.state == .unknown || reserved.state == .cancelled {
                    if reserved.needsRecovery { checkpoint = try journal.advance(checkpoint, to: .unresolved) }
                    try recordResult(reserved)
                    if reserved.state == .accepted && reserved.archiveConfirmed { return reserved }
                    throw DeviceSMSPendingError(reason: reserved.statusText)
                }
                guard reserved.state == .reserved || reserved.state == .prepared else { throw DeviceSMSIdentityRefusal() }
                // Reserve confirmation is durable before preparing or claiming.
                checkpoint = try journal.advance(checkpoint, to: reserved.state == .reserved ? .reserved : .prepared)
                try recordResult(reserved)
                // A recovered prepared ACK requires a separate explicit resume.
                if reserved.state == .prepared && !allowPrepared { throw DeviceSMSPendingError(reason: "原绑定已恢复，请明确继续原请求。") }
            }
            guard current(generation) else { throw CancellationError() }
            let prepared = try await api.prepareV2(bytes: checkpoint.sendBytes)
            guard current(generation) else { throw CancellationError() }
            guard prepared.matches(request) else { throw DeviceSMSIdentityRefusal() }
            try recordResult(prepared)
            if prepared.state == .accepted && prepared.archiveConfirmed { return prepared }
            guard prepared.state == .prepared && prepared.canSubmit else {
                if prepared.state == .unknown || prepared.state == .accepted { checkpoint = try journal.advance(checkpoint, to: .unresolved) }
                throw DeviceSMSPendingError(reason: "原请求已提交或绑定失效，未再次提交。")
            }
            checkpoint = try journal.advance(checkpoint, to: .prepared)
            // Local process races cannot both gain dispatch permission. A lost
            // response leaves this phase unresolved and only GET may follow.
            checkpoint = try journal.advance(checkpoint, to: .dispatched)
            guard current(generation) else { throw CancellationError() }
            try await api.submitV2(bytes: checkpoint.sendBytes)
            guard current(generation) else { throw CancellationError() }
            let result = try await api.get(id: request.clientRequestID)
            guard current(generation) else { throw CancellationError() }
            guard result.matches(request) else { throw DeviceSMSIdentityRefusal() }
            try recordResult(result)
            notice = result.statusText
            guard result.state == .accepted && result.archiveConfirmed else { throw DeviceSMSPendingError(reason: result.statusText) }
            return result
        } catch {
            guard current(generation) else { throw CancellationError() }
            if error is DeviceSMSIdentityRefusal { _ = try? journal.advance(checkpoint, to: .unresolved) }
            if let record = try? await api.get(id: request.clientRequestID), current(generation), record.matches(request) {
                if record.state == .unknown || (record.state == .accepted && !record.archiveConfirmed) {
                    _ = try? journal.advance(checkpoint, to: .unresolved)
                } else if record.state == .prepared {
                    _ = try? journal.advance(checkpoint, to: .prepared)
                }
                try recordResult(record)
                notice = record.statusText
                if record.state == .accepted && record.archiveConfirmed { return record }
            }
            guard current(generation) else { throw CancellationError() }
            self.error = "原签发候选及业务内容已保留；结果待确认时不会自动改号或重发。"
            throw DeviceSMSPendingError(reason: self.error!)
        }
    }

    private func recordResult(_ record: DeviceSMSIntent) throws {
        try journal?.confirmTerminal(record)
        issuancePhases = try Dictionary(uniqueKeysWithValues: (journal?.entries() ?? []).map { (try $0.request.clientRequestID, $0.phase) })
        queryRevision &+= 1
        loading = false
        recoveryRecords.removeAll { $0.id == record.id }
        if record.needsRecovery { recoveryRecords.append(record) }

        publishRecoveryRecords()
    }
}
