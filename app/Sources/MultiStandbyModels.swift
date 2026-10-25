import Foundation

struct MultiStandbyCard: Decodable, Identifiable, Equatable {
    let cardID: String
    let reader: String
    let eid: String?
    let profileCount: Int
    let available: Bool
    let detail: String?
    var id: String { cardID }
    enum CodingKeys: String, CodingKey {
        case cardID = "card_id", reader, eid, profileCount = "profile_count", available, detail
    }
}

struct MultiStandbyProfile: Decodable, Identifiable, Equatable {
    let profileID: String
    let cardID: String
    let displayName: String?
    let phoneNumber: String?
    let iccid: String?
    let physicalEnabled: Bool?
    let desiredState: String?
    let authState: String?
    let tunnelState: String?
    let imsState: String?
    let smsReady: Bool?
    let health: String?
    let lastError: String?
    let sessionGeneration: UInt64?
    let recoveryReason: String?
    let consecutiveFailures: Int?
    let nextRetryAtMS: Int64?
    let registrationExpiresAtMS: Int64?
    let registrationRenewAtMS: Int64?
    var id: String { profileID }
    var title: String {
        if let displayName, !displayName.isEmpty { return displayName }
        if let iccid, !iccid.isEmpty { return "Profile · " + iccid.suffix(4) }
        return "eSIM Profile"
    }
    var isReady: Bool { smsReady == true && ["keep_online", "running"].contains(desiredState ?? "") }
    var isStopped: Bool { desiredState == nil || desiredState == "stopped" }
    func label(masked: Bool) -> String {
        guard let phoneNumber, !phoneNumber.isEmpty else {
            if let displayName, !displayName.isEmpty, let iccid, !iccid.isEmpty {
                return title + " · ICCID 尾号 " + iccid.suffix(4)
            }
            return title
        }
        return title + " · " + (masked ? PrivacyMask.value(phoneNumber, enabled: true) : phoneNumber)
    }
    enum CodingKeys: String, CodingKey {
        case profileID = "profile_id", cardID = "card_id", displayName = "display_name"
        case phoneNumber = "phone_number", iccid, physicalEnabled = "physical_enabled"
        case desiredState = "desired_state", authState = "auth_state", tunnelState = "tunnel_state"
        case imsState = "ims_state", smsReady = "sms_ready", health, lastError = "last_error"
        case sessionGeneration = "session_generation"
        case recoveryReason = "recovery_reason", consecutiveFailures = "consecutive_failures"
        case nextRetryAtMS = "next_retry_at_ms", registrationExpiresAtMS = "registration_expires_at_ms"
        case registrationRenewAtMS = "registration_renew_at_ms"
    }
}

struct MultiStandbyMessage: Decodable, Identifiable, Equatable {
    let messageID: String
    let profileID: String
    let direction: String
    let peer: String
    let body: String
    let createdAtMS: Int64
    let state: String
    let kind: String?
    var revision: UInt64? = nil
    var id: String { profileID + ":" + messageID }
    var date: Date { Date(timeIntervalSince1970: Double(createdAtMS) / 1000) }
    var isIncoming: Bool { ["inbound", "in"].contains(direction) && (kind == nil || kind == "message") && state == "received" }
    var isOutgoing: Bool { ["outbound", "out"].contains(direction) }
    enum CodingKeys: String, CodingKey {
        case messageID = "message_id", profileID = "profile_id", direction, peer, body
        case createdAtMS = "created_at_ms", state, kind, revision
    }
}

/// Rust cursors are opaque to the UI. Accept numeric fixtures as well as strings.
struct MultiStandbyCursor: Decodable, Equatable {
    let value: String
    init(_ value: String) { self.value = value }
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(String.self) { self.value = value }
        else { value = String(try container.decode(UInt64.self)) }
    }
}

struct MultiStandbyCardsResponse: Decodable {
    let cards: [MultiStandbyCard]
    let bootID: String
    let detail: String?
    enum CodingKeys: String, CodingKey { case cards, bootID = "boot_id", detail }
}
struct MultiStandbyProfilesResponse: Decodable {
    let profiles: [MultiStandbyProfile]
    let bootID: String
    let revision: UInt64
    enum CodingKeys: String, CodingKey { case profiles, bootID = "boot_id", revision }
}
struct MultiStandbyMessagesResponse: Decodable {
    let messages: [MultiStandbyMessage]
    let nextCursor: MultiStandbyCursor?
    let revision: UInt64
    enum CodingKeys: String, CodingKey { case messages, nextCursor = "next_cursor", revision }
}
struct MultiStandbyConversation: Decodable, Identifiable, Equatable {
    let conversationID: String
    let profileID: String
    let peer: String
    let messageCount: Int
    let latestMessage: MultiStandbyMessage
    var id: String { conversationID }
    enum CodingKeys: String, CodingKey {
        case conversationID = "conversation_id", profileID = "profile_id", peer
        case messageCount = "message_count", latestMessage = "latest_message"
    }
}
struct MultiStandbyConversationsResponse: Decodable {
    let conversations: [MultiStandbyConversation]
    let nextCursor: MultiStandbyCursor?
    let revision: UInt64
    enum CodingKeys: String, CodingKey { case conversations, nextCursor = "next_cursor", revision }
}
struct MultiStandbyChange: Decodable {
    let revision: UInt64
    let profileID: String
    let messageID: String
    let direction: String?
    let kind: String?
    let state: String?
    let deleted: Bool?
    var conversationID: String? = nil
    enum CodingKeys: String, CodingKey {
        case revision, profileID = "profile_id", messageID = "message_id", direction, kind, state, deleted
        case conversationID = "conversation_id"
    }
}
struct MultiStandbyChangesResponse: Decodable {
    let changes: [MultiStandbyChange]
    let nextCursor: MultiStandbyCursor?
    let revision: UInt64?
    enum CodingKeys: String, CodingKey { case changes, nextCursor = "next_cursor", revision }
}
struct MultiStandbyControlRequest: Encodable {
    let cardID: String
    let profileIDs: [String]?
    let all: Bool?
    let clientRequestID: String
    enum CodingKeys: String, CodingKey {
        case cardID = "card_id", profileIDs = "profile_ids", all, clientRequestID = "client_request_id"
    }
}
struct MultiStandbyControlIntentRequest: Encodable, Equatable {
    let action: String
    let cardID: String
    let profileIDs: [String]?
    let all: Bool?
    enum CodingKeys: String, CodingKey {
        case action, cardID = "card_id", profileIDs = "profile_ids", all
    }
}
struct MultiStandbyPreparedControlIntent: Decodable {
    let clientRequestID: String
    enum CodingKeys: String, CodingKey { case clientRequestID = "client_request_id" }
}
struct MultiStandbyControlIntentAcknowledgement: Encodable {
    let action: String
    let cardID: String
    let profileIDs: [String]?
    let all: Bool?
    let clientRequestID: String
    enum CodingKeys: String, CodingKey {
        case action, cardID = "card_id", profileIDs = "profile_ids", all
        case clientRequestID = "client_request_id"
    }
}
struct MultiStandbyControlIntentAcknowledged: Decodable { let acknowledged: Bool }
struct MultiStandbyAccepted: Decodable {
    let operationID: String
    let accepted: Bool
    enum CodingKeys: String, CodingKey { case operationID = "operation_id", accepted }
}
struct MultiStandbyOperation: Decodable, Identifiable {
    let operationID: String
    let state: String
    let detail: String?
    var id: String { operationID }
    var isFinished: Bool { ["accepted", "completed", "failed", "cancelled", "partial", "stopped", "unknown"].contains(state) }
    var succeeded: Bool { ["accepted", "completed", "stopped"].contains(state) }
    enum CodingKeys: String, CodingKey { case operationID = "operation_id", state, detail }
}
struct MultiStandbySendRequest: Encodable, Equatable {
    let profileID: String
    let phone: String
    let body: String
    let clientRequestID: String
    enum CodingKeys: String, CodingKey {
        case profileID = "profile_id", phone, body, clientRequestID = "client_request_id"
    }
}
struct MultiStandbySent: Decodable {
    let messageID: String
    let state: String
    let revision: UInt64
    enum CodingKeys: String, CodingKey { case messageID = "message_id", state, revision }
}
struct MultiStandbySendIntent: Decodable, Identifiable {
    let profileID: String
    let clientRequestID: String
    let phone: String
    let body: String
    let phase: String
    let state: String
    let createdAtMS: Int64
    let messageID: String?
    let revision: UInt64?
    let canSubmit: Bool
    var id: String { profileID + ":" + clientRequestID }
    var request: MultiStandbySendRequest {
        MultiStandbySendRequest(profileID: profileID, phone: phone, body: body, clientRequestID: clientRequestID)
    }
    var needsRecovery: Bool { state == "prepared" || state == "unknown" }
    enum CodingKeys: String, CodingKey {
        case profileID = "profile_id", clientRequestID = "client_request_id", phone, body, phase, state
        case createdAtMS = "created_at_ms", messageID = "message_id", revision, canSubmit = "can_submit"
    }
}
struct MultiStandbySendIntentsResponse: Decodable {
    let intents: [MultiStandbySendIntent]
    let nextCursor: MultiStandbyCursor?
    enum CodingKeys: String, CodingKey { case intents, nextCursor = "next_cursor" }
}

@MainActor
protocol MultiStandbyAPI {
    func cards() async throws -> MultiStandbyCardsResponse
    func profiles(cardID: String) async throws -> MultiStandbyProfilesResponse
    func messages(profileID: String, cursor: String?) async throws -> MultiStandbyMessagesResponse
    func history(profileID: String, peer: String?, cursor: String?) async throws -> MultiStandbyMessagesResponse
    func conversations(profileID: String?, conversationID: String?, cursor: String?) async throws -> MultiStandbyConversationsResponse
    func changes(cursor: String?) async throws -> MultiStandbyChangesResponse
    func prepareControlIntent(_ request: MultiStandbyControlIntentRequest) async throws -> MultiStandbyPreparedControlIntent
    func acknowledgeControlIntent(_ request: MultiStandbyControlIntentAcknowledgement) async throws -> MultiStandbyControlIntentAcknowledged
    func control(start: Bool, request: MultiStandbyControlRequest) async throws -> MultiStandbyAccepted
    func retry(request: MultiStandbyControlRequest) async throws -> MultiStandbyAccepted
    func runtimeReport(sinceMS: Int64, cardID: String) async throws -> MultiStandbyRuntimeReport
    func operation(id: String) async throws -> MultiStandbyOperation
    func send(_ request: MultiStandbySendRequest) async throws -> MultiStandbySent
    func prepareSend(_ request: MultiStandbySendRequest) async throws -> MultiStandbySendIntent
    func authorizeSend(_ request: MultiStandbySendRequest) async throws -> MultiStandbySendIntent
    func sendIntent(profileID: String, clientRequestID: String) async throws -> MultiStandbySendIntent
    func sendIntents(cursor: String?) async throws -> MultiStandbySendIntentsResponse
    func delete(messageID: String, profileID: String) async throws
}

@MainActor
struct MultiStandbyHTTPAPI: MultiStandbyAPI {
    private let client = APIClient()
    private func path(_ endpoint: String, _ query: [(String, String?)] = []) -> String {
        var components = URLComponents()
        components.queryItems = query.compactMap { name, value in
            guard let value, !value.isEmpty else { return nil }
            return URLQueryItem(name: name, value: value)
        }
        let suffix = components.percentEncodedQuery.map { "?" + $0 } ?? ""
        return "api/multistandby/" + endpoint + suffix
    }
    func cards() async throws -> MultiStandbyCardsResponse { try await client.get(path("cards")) }
    func profiles(cardID: String) async throws -> MultiStandbyProfilesResponse {
        try await client.get(path("profiles", [("card_id", cardID)]))
    }
    func messages(profileID: String, cursor: String?) async throws -> MultiStandbyMessagesResponse {
        try await client.get(path("messages", [("profile_id", profileID), ("cursor", cursor)]))
    }
    func history(profileID: String, peer: String?, cursor: String?) async throws -> MultiStandbyMessagesResponse {
        let peerHex = peer.map { $0.utf8.map { String(format: "%02x", $0) }.joined() }
        return try await client.get(path("history", [("profile_id", profileID), ("peer_hex", peerHex), ("cursor", cursor)]))
    }
    func conversations(profileID: String?, conversationID: String?, cursor: String?) async throws -> MultiStandbyConversationsResponse {
        try await client.get(path("conversations", [("profile_id", profileID), ("conversation_id", conversationID), ("cursor", cursor)]))
    }
    func changes(cursor: String?) async throws -> MultiStandbyChangesResponse {
        try await client.get(path("changes", [("cursor", cursor)]))
    }
    func prepareControlIntent(_ request: MultiStandbyControlIntentRequest) async throws -> MultiStandbyPreparedControlIntent {
        try await client.send(path("control-intents"), body: request)
    }
    func acknowledgeControlIntent(_ request: MultiStandbyControlIntentAcknowledgement) async throws -> MultiStandbyControlIntentAcknowledged {
        try await client.send(path("control-intents/ack"), body: request)
    }
    func control(start: Bool, request: MultiStandbyControlRequest) async throws -> MultiStandbyAccepted {
        try await client.send(path(start ? "start" : "stop"), body: request)
    }
    func retry(request: MultiStandbyControlRequest) async throws -> MultiStandbyAccepted {
        try await client.send(path("retry"), body: request)
    }
    func runtimeReport(sinceMS: Int64, cardID: String) async throws -> MultiStandbyRuntimeReport {
        try await client.get(path("report", [("since_ms", String(sinceMS)), ("card_id", cardID)]))
    }
    func operation(id: String) async throws -> MultiStandbyOperation {
        try await client.get(path("operations/" + id))
    }
    func send(_ request: MultiStandbySendRequest) async throws -> MultiStandbySent {
        try await client.send(path("messages/send"), body: request)
    }
    func prepareSend(_ request: MultiStandbySendRequest) async throws -> MultiStandbySendIntent {
        try await client.send(path("send-intents"), body: request)
    }
    func authorizeSend(_ request: MultiStandbySendRequest) async throws -> MultiStandbySendIntent {
        try await client.send(path("send-intents/submit"), body: request)
    }
    func sendIntent(profileID: String, clientRequestID: String) async throws -> MultiStandbySendIntent {
        try await client.get(path("send-intents", [("profile_id", profileID), ("client_request_id", clientRequestID)]))
    }
    func sendIntents(cursor: String?) async throws -> MultiStandbySendIntentsResponse {
        try await client.get(path("send-intents", [("cursor", cursor)]))
    }
    func delete(messageID: String, profileID: String) async throws {
        try await client.send(path("messages/" + messageID, [("profile_id", profileID)]), method: "DELETE")
    }
}

struct MultiStandbyRuntimeEvent: Decodable, Identifiable {
    let atMS: Int64
    let profileID: String
    let generation: UInt64
    let state: String
    let reason: String
    let recovery: Bool?
    var id: String { "\(atMS):\(profileID):\(generation):\(reason):\(state)" }
    enum CodingKeys: String, CodingKey {
        case atMS = "at_ms", profileID = "profile_id", generation, state, reason, recovery
    }
}
struct MultiStandbyRuntimeProfileReport: Decodable, Identifiable {
    let profileID: String
    let onlineMS: Int64
    let notReadyMS: Int64
    let stoppedMS: Int64
    let unknownMS: Int64
    let plannedRenewals: Int
    let plannedIKEReauths: Int
    let unexpectedFailures: Int
    let manualRetries: Int
    let slowestRecoveryMS: Int64
    let state: String
    let sinceMS: Int64
    let lastObservedMS: Int64
    var id: String { profileID }
    enum CodingKeys: String, CodingKey {
        case profileID = "profile_id", onlineMS = "online_ms", notReadyMS = "not_ready_ms"
        case stoppedMS = "stopped_ms", unknownMS = "unknown_ms", plannedRenewals = "planned_renewals"
        case plannedIKEReauths = "planned_ike_reauths"
        case unexpectedFailures = "unexpected_failures", manualRetries = "manual_retries"
        case slowestRecoveryMS = "slowest_recovery_ms", state, sinceMS = "since_ms", lastObservedMS = "last_observed_ms"
    }
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        profileID = try values.decode(String.self, forKey: .profileID)
        onlineMS = try values.decode(Int64.self, forKey: .onlineMS)
        notReadyMS = try values.decode(Int64.self, forKey: .notReadyMS)
        stoppedMS = try values.decode(Int64.self, forKey: .stoppedMS)
        unknownMS = try values.decode(Int64.self, forKey: .unknownMS)
        plannedRenewals = try values.decode(Int.self, forKey: .plannedRenewals)
        plannedIKEReauths = try values.decodeIfPresent(Int.self, forKey: .plannedIKEReauths) ?? 0
        unexpectedFailures = try values.decode(Int.self, forKey: .unexpectedFailures)
        manualRetries = try values.decode(Int.self, forKey: .manualRetries)
        slowestRecoveryMS = try values.decode(Int64.self, forKey: .slowestRecoveryMS)
        state = try values.decode(String.self, forKey: .state)
        sinceMS = try values.decode(Int64.self, forKey: .sinceMS)
        lastObservedMS = try values.decode(Int64.self, forKey: .lastObservedMS)
    }
}
struct MultiStandbyRuntimeReport: Decodable {
    let sinceMS: Int64
    let untilMS: Int64
    let coverageStartMS: Int64
    let retentionDays: Int
    let eventLimit: Int
    let events: [MultiStandbyRuntimeEvent]
    let profiles: [MultiStandbyRuntimeProfileReport]
    let storageIssue: String?
    enum CodingKeys: String, CodingKey {
        case sinceMS = "since_ms", untilMS = "until_ms", coverageStartMS = "coverage_start_ms"
        case retentionDays = "retention_days", eventLimit = "event_limit", events, profiles, storageIssue = "storage_issue"
    }
}

enum MultiStandbyProfileFilter: String, CaseIterable, Identifiable {
    case all = "全部", online = "在线", recovering = "待恢复", stopped = "已停止"
    var id: String { rawValue }
}

enum MultiStandbyPresentation {
    static func profiles(_ profiles: [MultiStandbyProfile], query: String, filter: MultiStandbyProfileFilter,
                         anomaliesFirst: Bool, masked: Bool) -> [MultiStandbyProfile] {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return profiles.filter { profile in
            let matchesState: Bool
            switch filter {
            case .all: matchesState = true
            case .online: matchesState = profile.isReady
            case .recovering: matchesState = !profile.isReady && !profile.isStopped
            case .stopped: matchesState = profile.isStopped
            }
            return matchesState && (query.isEmpty || profile.label(masked: masked).localizedCaseInsensitiveContains(query)
                || (profile.iccid.map { String($0.suffix(4)).contains(query) } ?? false))
        }.sorted { lhs, rhs in
            if anomaliesFirst {
                let l = !lhs.isReady && !lhs.isStopped
                let r = !rhs.isReady && !rhs.isStopped
                if l != r { return l }
            }
            let comparison = lhs.title.localizedStandardCompare(rhs.title)
            return comparison == .orderedSame ? lhs.profileID < rhs.profileID : comparison == .orderedAscending
        }
    }
    static func reason(_ reason: String?) -> String {
        switch reason {
        case "planned_renewal": return "计划续期"
        case "planned_ike_reauth": return "计划 IKE 重认证"
        case "manual_retry": return "手动重连"
        case "tunnel_lost": return "隧道中断"
        case "ims_lost": return "IMS 通道中断"
        case "startup_failed": return "连接失败"
        case "reader_lost": return "读卡器断开"
        case "manual_stop": return "手动停止"
        case "backend_exit", "backend_restart": return "服务进程间隔"
        case "observation_gap": return "观测暂停，期间状态未知"
        case "clock_adjustment": return "系统时钟调整，期间状态未知"
        case "registered": return "注册完成"
        case "start_requested", "starting": return "启动连接"
        case "readiness", "heartbeat": return "状态检查"
        default: return "等待状态"
        }
    }
    static func duration(_ milliseconds: Int64) -> String {
        let seconds = max(0, milliseconds) / 1000
        if seconds >= 3600 { return "\(seconds / 3600)时\((seconds % 3600) / 60)分" }
        if seconds >= 60 { return "\(seconds / 60)分\(seconds % 60)秒" }
        return "\(seconds)秒"
    }
}
