import Foundation
import CryptoKit

/// Stable conversation keys keep identical peers on separate sending lines.
/// Only opaque identifiers, never SMS bodies or phone numbers, are persisted.
enum SMSConversationIdentity {
    static func digest(_ value: String) -> String {
        SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    static func device(lineID: String, peer: String, subscriptionID: String? = nil) -> String {
        let legacy = lineID.lowercased() + "\u{0}" + peer
        guard let subscription = normalizedSubscription(subscriptionID) else {
            return "device|" + digest(legacy)
        }
        return "device|" + digest("subscription-v1\u{0}" + subscription + "\u{0}" + legacy)
    }
    static func normalizedSubscription(_ value: String?) -> String? {
        guard let value, value.utf8.count == 69, value.hasPrefix("sub1_"),
              value.dropFirst(5).allSatisfy({ ("0"..."9").contains(String($0)) || ("a"..."f").contains(String($0)) }) else { return nil }
        return value
    }
    static func multiStandby(profileID: String, peer: String) -> String {
        "multistandby|" + digest(profileID + "\u{0}" + peer)
    }
}

// MARK: - 短信

/// GET /api/sms：顶层是数组
struct SMSItem: Decodable, Identifiable {
    /// 后端生成的稳定逻辑消息 ID，用于精确删除；旧后端可能不提供。
    let backendID: String?
    let sender: String?
    let content: String?
    let code: String?
    let timestamp: Date
    let moduleStorage: String?
    let moduleIndex: Int?
    let archived: Bool?
    let direction: String?
    let device: String?
    var subscriptionID: String? = nil
    var sendRequestID: String? = nil
    var deletionToken: String? = nil

    var subscription: String? { SMSConversationIdentity.normalizedSubscription(subscriptionID) }

    var id: String {
        let legacy = "\(device ?? "legacy")|\(timestamp.timeIntervalSince1970)|\(sender ?? "")|\(content ?? "")"
        let scoped = subscription.map { "subscription|\($0)|" + legacy } ?? legacy
        if isOutgoing, let sendRequestID, !sendRequestID.isEmpty { return "send-request|" + sendRequestID + "|" + scoped }
        return scoped
    }
    var lineID: String { device?.lowercased() ?? "legacy" }
    var threadID: String { SMSConversationIdentity.device(lineID: lineID, peer: sender ?? "未知号码", subscriptionID: subscription) }
    var notificationID: String { SMSConversationIdentity.digest(id) }
    var forwardingID: String { "device:" + (backendID ?? notificationID) }
    /// 短信当前仍在模块/SIM 存储中（可精确定位删除）
    var isFromModule: Bool { (moduleIndex ?? 0) > 0 && archived != true }
    /// 已持久化到本机
    var isArchived: Bool { archived == true }
    /// 是否为发送的短信
    var isOutgoing: Bool { direction == "out" }

    enum CodingKeys: String, CodingKey {
        case backendID = "id"
        case sender, content, code, timestamp, archived, direction, device
        case moduleStorage = "module_storage"
        case moduleIndex = "module_index"
        case subscriptionID = "subscription_id"
        case sendRequestID = "send_request_id"
        case deletionToken = "deletion_token"
    }
}

/// GET /api/sms/storage
struct SMSStorageUsage: Decodable {
    let used: Int
    let total: Int
}

struct SMSStorageResponse: Decodable {
    let usage: [String: SMSStorageUsage]?
}

/// POST /api/sms/delete
struct SMSDeleteRequest: Encodable {
    let id: String?
    let storage: String?
    let index: Int
    let sender: String?
    let content: String?
    let timestamp: Date?
    let deletionToken: String?

    enum CodingKeys: String, CodingKey {
        case id, storage, index, sender, content, timestamp
        case deletionToken = "deletion_token"
    }

    init(
        id: String? = nil,
        storage: String? = nil,
        index: Int = 0,
        sender: String? = nil,
        content: String? = nil,
        timestamp: Date? = nil,
        deletionToken: String? = nil
    ) {
        self.id = id
        self.storage = storage
        self.index = index
        self.sender = sender
        self.content = content
        self.timestamp = timestamp
        self.deletionToken = deletionToken
    }
}

struct SMSDeleteResult: Decodable {
    let deleted: Bool
}

/// POST /api/sms/delete-sender
struct SMSDeleteSenderRequest: Encodable {
    let sender: String
}

struct SMSDeleteSenderResult: Decodable {
    let deleted: Bool
    let deletedCount: Int

    enum CodingKeys: String, CodingKey {
        case deleted
        case deletedCount = "deleted_count"
    }
}

/// POST /api/sms/adopt
struct SMSAdoptRequest: Encodable {
    let enabled: Bool
}

/// POST /api/sms/clear
struct SMSClearRequest: Encodable {
    let sm: Bool
    let me: Bool
    let local: Bool
}

struct SMSClearResult: Decodable {
    let cleared: Bool
}

/// GET /api/sms/status
struct SMSStatus: Decodable {
    let count: Int?
    let polling: Bool?
    let pollIntervalS: Int?
    /// 兼容旧后端的可选字段；不作为接管后自动清空存储的 UI 承诺。
    let autoCleanupME: Bool?
    let lastPoll: Date?
    let lastPollError: String?

    enum CodingKeys: String, CodingKey {
        case count, polling
        case pollIntervalS = "poll_interval_s"
        case autoCleanupME = "auto_cleanup_me"
        case lastPoll = "last_poll"
        case lastPollError = "last_poll_error"
    }
}

/// POST /api/sms/send
struct SMSSendRequest: Encodable {
    let phone: String
    let message: String
    let device: String?

    init(phone: String, message: String, device: String? = nil) {
        self.phone = phone
        self.message = message
        self.device = device
    }
}

struct SMSSendResult: Decodable {
    let sent: Bool
    let segments: Int?
    let device: String?
}

struct SMSDeviceStatus: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let kind: String?
    let phone: String?
    let available: Bool
    let ready: Bool
    let detail: String?
    var subscriptionID: String? = nil

    var subscription: String? { SMSConversationIdentity.normalizedSubscription(subscriptionID) }

    /// Picker identity changes with the observed subscription, so a draft does
    /// not silently keep the same physical reader after a card switch.
    var selectionID: String { id + "|" + (subscription ?? "unknown") }

    static func replySelection(lineID: String, subscriptionID: String?, devices: [SMSDeviceStatus]) -> String {
        guard let subscription = SMSConversationIdentity.normalizedSubscription(subscriptionID) else { return "" }
        return devices.first { $0.ready && $0.id == lineID && $0.subscription == subscription }?.selectionID ?? ""
    }

    static func historyLabel(lineID: String, subscriptionID: String?, devices: [SMSDeviceStatus], masked: Bool) -> String {
        let subscription = SMSConversationIdentity.normalizedSubscription(subscriptionID)
        if let subscription, let current = devices.first(where: { $0.id == lineID && $0.subscription == subscription }) {
            return current.displayName(privacyMaskEnabled: masked)
        }
        let source = lineID == "reader" ? "Reader" : (lineID == "dji" ? "DJI" : "设备")
        return (subscription == nil ? "历史线路 · 归属未知 · " : "历史线路 · ") + source
    }

    enum CodingKeys: String, CodingKey {
        case id, name, kind, phone, available, ready, detail
        case subscriptionID = "subscription_id"
    }

    func displayName(privacyMaskEnabled: Bool) -> String {
        guard let phone = phone?.trimmingCharacters(in: .whitespacesAndNewlines),
              !phone.isEmpty else {
            return name
        }
        let suffix: String
        switch CommunicationDeviceID(rawValue: (kind ?? id).lowercased()) {
        case .reader: suffix = "Reader"
        case .dji: suffix = "DJI"
        case nil: suffix = kind?.trimmingCharacters(in: .whitespacesAndNewlines) ?? id
        }
        return "\(PrivacyMask.value(phone, enabled: privacyMaskEnabled))_\(suffix)"
    }
}

enum CommunicationDeviceID: String, CaseIterable {
    case dji
    case reader

    var displayOrder: Int {
        switch self {
        case .dji: return 0
        case .reader: return 1
        }
    }
}

/// 双设备同时在线时的稳定选择规则。设备列表的网络返回顺序不会影响用户选择，
/// 已选择且仍就绪的设备始终保留；仅在其失效时才回退到固定顺序的可用设备。
struct SMSDeviceCatalog {
    let devices: [SMSDeviceStatus]

    var readyDevices: [SMSDeviceStatus] {
        devices
            .filter(\.ready)
            .sorted { lhs, rhs in
                let left = CommunicationDeviceID(rawValue: lhs.id)?.displayOrder ?? Int.max
                let right = CommunicationDeviceID(rawValue: rhs.id)?.displayOrder ?? Int.max
                if left != right { return left < right }
                return lhs.id.localizedStandardCompare(rhs.id) == .orderedAscending
            }
    }

    func reconciledSelection(_ current: String) -> String {
        let ready = readyDevices
        guard !ready.isEmpty else { return "" }
        if ready.contains(where: { $0.id == current }) {
            return current
        }
        return ready[0].id
    }
}

struct SMSDevicesResponse: Decodable {
    let devices: [SMSDeviceStatus]
}

/// POST /api/sms/refresh（202）
struct SMSRefreshResult: Decodable {
    let accepted: Bool
    let count: Int?
}
