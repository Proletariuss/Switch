import Combine
import CryptoKit
import Darwin
import Foundation
import Security

@_silgen_name("switch_forwarding_publish_empty_queue")
private func rustPublishEmptyForwardingQueue(_ directory: Int32, _ key: UnsafePointer<UInt8>?,
                                             _ length: Int) -> Int32

@_silgen_name("switch_forwarding_diagnose_queue")
private func rustDiagnoseForwardingQueue(_ directory: Int32, _ keys: UnsafePointer<UInt8>?,
                                        _ count: Int, _ results: UnsafeMutablePointer<UInt8>?,
                                        _ observation: UnsafeMutablePointer<UnsafeMutableRawPointer?>) -> Int32
@_silgen_name("switch_forwarding_diagnostic_confirm")
private func rustConfirmForwardingDiagnosis(_ observation: UnsafeRawPointer?) -> Int32
@_silgen_name("switch_forwarding_diagnostic_destroy")
private func rustDestroyForwardingDiagnosis(_ observation: UnsafeMutableRawPointer?)

@_silgen_name("switch_forwarding_recovery_begin")
private func rustBeginForwardingRecovery(_ directory: Int32, _ key: UnsafePointer<UInt8>?, _ length: Int,
                                        _ proof: UnsafeMutablePointer<UnsafeMutableRawPointer?>) -> Int32
@_silgen_name("switch_forwarding_recovery_prepare_witness")
private func rustPrepareForwardingRecoveryWitness(_ proof: UnsafeMutableRawPointer?) -> Int32
@_silgen_name("switch_forwarding_recovery_confirm")
private func rustConfirmForwardingRecovery(_ proof: UnsafeMutableRawPointer?) -> Int32
@_silgen_name("switch_forwarding_recovery_destroy")
private func rustDestroyForwardingRecovery(_ proof: UnsafeMutableRawPointer?)

/// Redacted observation only. A matching stage is not active authority and this
/// value cannot be supplied to any save, configure, queue or recovery operation.
struct SMSForwardingRecoveryDiagnosis {
    enum State: String {
        case noSeed, tornSeed, unsupportedQueue, unmatchedQueue, legacyKeyQueue
        case uniqueStagedSeed, uniqueStagedQueue, activeQueue, ambiguousCandidates
        case unusableQueue, invalidSnapshot, busy, unsafeNamespace, unavailable
    }
    enum Witness: String { case missing, matching, mismatched, malformed, unverified }
    let state: State
    let witness: Witness
    let snapshotCount: Int
    let activePointerPresent: Bool
}

struct SMSForwardingTelegramConfiguration: Codable, Sendable {
    var enabled: Bool
    var botToken: String
    var chatID: String
    var proxy: SMSForwardingProxyConfiguration
    enum CodingKeys: String, CodingKey { case enabled, botToken = "bot_token", chatID = "chat_id", proxy }
}

struct SMSForwardingProxyConfiguration: Codable, Sendable {
    var mode: String
    var url: String
}

struct SMSForwardingBarkConfiguration: Codable, Sendable {
    var enabled: Bool
    var serverURL: String
    var deviceKey: String
    var iconURL: String
    var group: String
    var encrypted: Bool
    var encryptionAlgorithm: String
    var encryptionKey: String
    enum CodingKeys: String, CodingKey {
        case enabled, serverURL = "server_url", deviceKey = "device_key", iconURL = "icon_url"
        case group, encrypted, encryptionAlgorithm = "encryption_algorithm", encryptionKey = "encryption_key"
    }
}

private struct SMSForwardingConfiguration: Codable, Sendable {
    let telegram: SMSForwardingTelegramConfiguration
    let bark: SMSForwardingBarkConfiguration
    let stateKey: String
    enum CodingKeys: String, CodingKey { case telegram, bark, stateKey = "state_key" }
}

private struct SMSForwardingConfigurationIntent: Codable, Equatable, Sendable {
    let clientRequestID: String
    let clientSequence: UInt64
}

/// One immutable Keychain item contains every field and its Rust intent.
/// Only configuration.active.v1 selects which complete snapshot may run.
private struct SMSForwardingStoredConfiguration: Codable, Sendable {
    let schemaVersion: Int
    let configuration: SMSForwardingConfiguration
    let intent: SMSForwardingConfigurationIntent
}

private struct SMSForwardingConfigureRequest: Encodable {
    let clientRequestID: String
    let clientSequence: UInt64
    let telegram: SMSForwardingTelegramConfiguration
    let bark: SMSForwardingBarkConfiguration
    let stateKey: String
    enum CodingKeys: String, CodingKey {
        case clientRequestID = "client_request_id", clientSequence = "client_sequence"
        case telegram, bark, stateKey = "state_key"
    }

    init(configuration: SMSForwardingConfiguration, intent: SMSForwardingConfigurationIntent) {
        clientRequestID = intent.clientRequestID
        clientSequence = intent.clientSequence
        telegram = configuration.telegram
        bark = configuration.bark
        stateKey = configuration.stateKey
    }
}

struct SMSForwardingStatus: Decodable {
    let configurationID: String?
    let configured: Bool
    let telegramEnabled: Bool
    let barkEnabled: Bool
    let telegramProxy: String?
    let pending: Int
    let lastError: String?
    let telegram: SMSForwardingChannelSummary?
    let bark: SMSForwardingChannelSummary?
    let tasks: [SMSForwardingTaskStatus]?
    let tasksTruncated: Bool?
    enum CodingKeys: String, CodingKey {
        case configurationID = "configuration_id"
        case configured, telegramEnabled = "telegram_enabled", barkEnabled = "bark_enabled"
        case telegramProxy = "telegram_proxy", pending, lastError = "last_error", telegram, bark, tasks
        case tasksTruncated = "tasks_truncated"
    }
}

struct SMSForwardingChannelSummary: Decodable {
    let pending: Int
    let queued: Int
    let inFlight: Int
    let retrying: Int
    let paused: Int
    let delivered: Int
    let cancelled: Int
    let skipped: Int
    enum CodingKeys: String, CodingKey {
        case pending, queued, inFlight = "in_flight", retrying, paused, delivered, cancelled, skipped
    }
}

struct SMSForwardingChannelStatus: Decodable {
    let state: String
    let attempts: Int
    let revision: String
    let errorCode: String?
    let nextAttemptAt: Date?
    let lastAttemptAt: Date?
    let confirmedParts: Int?
    let totalParts: Int?
    enum CodingKeys: String, CodingKey {
        case state, attempts, revision, errorCode = "error_code", nextAttemptAt = "next_attempt_at", lastAttemptAt = "last_attempt_at"
        case confirmedParts = "confirmed_parts", totalParts = "total_parts"
    }
    var canRetry: Bool { ["retry_wait", "paused_configuration_error", "paused_result_unknown", "queued"].contains(state) }
    var requiresRetryConfirmation: Bool { state == "paused_result_unknown" }
    var canCancel: Bool { !["delivered", "cancelled", "skipped", "submitting"].contains(state) }
    var label: String {
        switch state {
        case "queued": return "等待发送"
        case "submitting": return "发送中"
        case "retry_wait": return "等待重试"
        case "paused_disabled": return "渠道已关闭，任务暂停"
        case "paused_destination_unverified": return "旧任务目标未确认"
        case "paused_destination_changed": return "目标已更改，请恢复原目标或取消任务"
        case "paused_unavailable": return "渠道暂不可用"
        case "paused_configuration_error": return "请修正渠道配置后重试"
        case "paused_result_unknown": return "服务接收结果未知，已暂停"
        case "paused_payload_mismatch": return "分片内容或版本不匹配，任务已暂停"
        case "delivered": return "服务已接收"
        case "cancelled": return "已取消"
        case "skipped": return "未启用此渠道"
        default: return "状态未知"
        }
    }
}

/// Provider codes are redacted by Rust, but only this allowlist is shown in
/// the UI. Unknown codes must never become user-visible provider text.
enum SMSForwardingFailurePresentation {
    static func message(_ code: String, provider: String) -> String {
        switch code {
        case "telegram_configuration_invalid":
            return "请检查 Telegram Bot Token 和 Chat ID。"
        case "telegram_system_proxy_not_found":
            return "未找到系统代理，请启用系统代理或选择自定义代理。"
        case "telegram_system_proxy_resolution_failed":
            return "系统代理无法解析，请检查代理设置或改用自定义代理。"
        case "telegram_proxy_url_invalid", "telegram_proxy_configuration_rejected":
            return "Telegram 代理配置无效，请检查协议、地址和端口。"
        case "telegram_proxy_auth_required":
            return "Telegram 代理要求身份认证，请检查代理账号和密码。"
        case "telegram_proxy_policy_rejected":
            return "Telegram 请求被代理策略拒绝，请检查分流规则。"
        case "telegram_proxy_unavailable", "telegram_connect_failed":
            return "无法连接 Telegram 代理或服务器，请检查代理线路。"
        case "telegram_network_error":
            return "Telegram 连接中断，服务是否收到通知尚不确定。"
        case "telegram_api_rejected", "telegram_http_400":
            return "Telegram 拒绝了请求，请核对 Chat ID 和 Bot 会话权限。"
        case "telegram_http_401":
            return "Telegram Bot Token 无效或已失效。"
        case "telegram_http_403":
            return "Telegram 禁止发送；请先向 Bot 发送 /start，并检查群组权限。"
        case "telegram_http_429":
            return "Telegram 请求过于频繁，请稍后重试。"
        case "telegram_not_enabled":
            return "Telegram 尚未启用，请保存配置后重试。"
        case "telegram_request_invalid":
            return "Telegram 请求无效，请重新保存配置。"
        case "telegram_response_invalid", "telegram_response_too_large", "telegram_response_read_failed":
            return "Telegram 响应无法确认，请先核对接收端。"
        case "bark_configuration_invalid", "bark_request_invalid":
            return "请检查 Bark 服务器地址和设备密钥。"
        case "bark_connect_failed", "bark_proxy_unavailable", "bark_network_error":
            return "无法连接 Bark 服务，请检查网络和服务器地址。"
        case "bark_proxy_auth_required":
            return "Bark 代理要求身份认证，请检查代理账号和密码。"
        case "bark_proxy_policy_rejected", "bark_proxy_configuration_rejected":
            return "Bark 请求被代理拒绝，请检查代理设置。"
        case "bark_payload_encode_failed", "bark_encryption_failed":
            return "Bark 通知加密或编码失败，请检查加密设置。"
        case "bark_not_enabled":
            return "Bark 尚未启用，请保存配置后重试。"
        case "bark_response_invalid", "bark_response_too_large", "bark_response_read_failed":
            return "Bark 响应无法确认，请先核对接收端。"
        case "http_client_unavailable":
            return "通知网络服务不可用，请稍后重试。"
        default:
            break
        }
        if let status = providerHTTPStatus(code, prefix: "telegram_http_"), provider == "telegram" {
            return "Telegram 返回 HTTP \(status)，请检查 Bot、代理与网络。"
        }
        if let status = providerHTTPStatus(code, prefix: "bark_http_"), provider == "bark" {
            return "Bark 返回 HTTP \(status)，请检查服务器与网络。"
        }
        if let status = providerHTTPStatus(code, prefix: "bark_api_"), provider == "bark" {
            return "Bark 服务返回状态 \(status)，请检查设备密钥与服务器。"
        }
        return provider == "telegram"
            ? "Telegram 通知未获确认，请检查代理、Bot 配置和接收端。"
            : "Bark 通知未获确认，请检查服务器配置和接收端。"
    }

    private static func providerHTTPStatus(_ code: String, prefix: String) -> Int? {
        guard code.hasPrefix(prefix) else { return nil }
        let suffix = code.dropFirst(prefix.count)
        guard suffix.count == 3, suffix.allSatisfy({ $0 >= "0" && $0 <= "9" }),
              let status = Int(suffix),
              (100...599).contains(status) else { return nil }
        return status
    }
}

struct SMSForwardingTaskStatus: Decodable, Identifiable {
    let id: String
    let telegram: SMSForwardingChannelStatus
    let bark: SMSForwardingChannelStatus
    let updatedAt: Date
    enum CodingKeys: String, CodingKey { case id, telegram, bark, updatedAt = "updated_at" }
}

private struct SMSForwardingProviderRequest: Codable, Equatable {
    let clientRequestID: String
    let provider: String
    let expectedConfigurationID: String
    enum CodingKeys: String, CodingKey {
        case clientRequestID = "client_request_id", provider, expectedConfigurationID = "expected_configuration_id"
    }
}
private struct SMSForwardingConfiguredResponse: Decodable {
    let configured: Bool
    let configurationID: String?
    let clientRequestID: String
    let clientSequence: UInt64
    let configurationIntentRevision: UInt64
    enum CodingKeys: String, CodingKey {
        case configured, configurationID = "configuration_id"
        case clientRequestID = "client_request_id", clientSequence = "client_sequence"
        case configurationIntentRevision = "configuration_intent_revision"
    }
}
struct SMSForwardingHandoffV1: Codable, Equatable {
    let schemaVersion: Int
    let handoffID: String
    let deviceSMS: DeviceSMSForwardingHandoff
    let nwaitSMS: NwaitSMSForwardingHandoff
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", handoffID = "handoff_id"
        case deviceSMS = "device_sms", nwaitSMS = "nwait_sms"
    }
}
struct DeviceSMSForwardingHandoff: Codable, Equatable {
    let acceptedNotificationIDs: [String]
    let wasEnabled: Bool?
    enum CodingKeys: String, CodingKey {
        case acceptedNotificationIDs = "accepted_notification_ids", wasEnabled = "was_enabled"
    }
}
struct NwaitSMSForwardingHandoff: Codable, Equatable {
    let cursor: String?
    let seeded: Bool
    let wasEnabled: Bool?
    enum CodingKeys: String, CodingKey { case cursor, seeded, wasEnabled = "was_enabled" }
}
private struct SMSForwardingLegacyProgress: Encodable {
    let deviceSMS: DeviceSMSForwardingHandoff
    let nwaitSMS: NwaitSMSForwardingHandoff
    enum CodingKeys: String, CodingKey { case deviceSMS = "device_sms", nwaitSMS = "nwait_sms" }
}
struct SMSForwardingHandoffAck: Decodable, Equatable {
    let accepted: Bool
    let handoffID: String
    let alreadyAccepted: Bool
    enum CodingKeys: String, CodingKey {
        case accepted, handoffID = "handoff_id", alreadyAccepted = "already_accepted"
    }
}
private struct SMSForwardingHandoffSnapshotResponse: Decodable {
    let accepted: Bool
    let snapshot: SMSForwardingHandoffV1?
}
private enum SMSForwardingHandoffError: Error {
    case tooManyAcceptedIDs
    case invalidAcceptedID
    case cursorTooLong
    case invalidFrozenSnapshot
    case snapshotPersistenceFailed
    case acknowledgementMismatch
    case handoffInProgress
}
private struct SMSForwardingTestResponse: Decodable {
    let clientRequestID: String
    let provider: String
    let expectedConfigurationID: String
    let state: String
    let terminal: Bool
    let sent: Bool
    let dispatchCount: Int
    let code: String
    enum CodingKeys: String, CodingKey {
        case clientRequestID = "client_request_id", provider
        case expectedConfigurationID = "expected_configuration_id"
        case state, terminal, sent, code, dispatchCount = "dispatch_count"
    }
}

private struct SMSForwardingPendingTest: Codable, Equatable {
    let request: SMSForwardingProviderRequest
    let configurationIntentID: String
}

/// Only caller intent bytes are kept here. Rust alone owns dispatch and final
/// outcomes. File+directory fsync must succeed before the first HTTP request.
private struct SMSForwardingTestCheckpoint {
    let directory: URL
    static let fileName = "sms-forwarding-pending-tests-v1.json"
    private var file: URL { directory.appendingPathComponent(Self.fileName) }
    func load() throws -> [String: SMSForwardingPendingTest] {
        guard FileManager.default.fileExists(atPath: file.path) else { return [:] }
        let bytes = try Data(contentsOf: file)
        guard bytes.count <= 64 * 1024 else { throw Failure.invalid }
        let pending = try JSONDecoder().decode([String: SMSForwardingPendingTest].self, from: bytes)
        guard pending.count <= 2, pending.allSatisfy({ provider, item in
            ["telegram", "bark"].contains(provider) && item.request.provider == provider
                && !item.request.clientRequestID.isEmpty && !item.request.expectedConfigurationID.isEmpty
                && !item.configurationIntentID.isEmpty
        }) else { throw Failure.invalid }
        return pending
    }
    func save(_ pending: [String: SMSForwardingPendingTest]) throws {
        let bytes = try JSONEncoder().encode(pending)
        try bytes.write(to: file, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
        let descriptor = open(file.path, O_RDONLY | O_NOFOLLOW)
        guard descriptor >= 0 else { throw Failure.unavailable }
        defer { close(descriptor) }
        guard fsync(descriptor) == 0 else { throw Failure.unavailable }
        let parent = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW)
        guard parent >= 0 else { throw Failure.unavailable }
        defer { close(parent) }
        guard fsync(parent) == 0 else { throw Failure.unavailable }
    }
    enum Failure: Error { case invalid, unavailable }
}
private struct SMSForwardingQueueActionRequest: Encodable {
    let id: String
    let provider: String
    let action: String
    let expectedConfigurationID: String
    let expectedAttempts: Int
    let expectedState: String
    let expectedDestinationRevision: String
    enum CodingKeys: String, CodingKey {
        case id, provider, action
        case expectedConfigurationID = "expected_configuration_id"
        case expectedAttempts = "expected_attempts"
        case expectedState = "expected_state"
        case expectedDestinationRevision = "expected_destination_revision"
    }
}
private struct SMSForwardingQueueActionResponse: Decodable { let accepted: Bool }

/// The v2 service isolates queue keys by canonical data root. One fixed,
/// real-user lock serializes every namespace and the read-only legacy migration.
private struct SMSForwardingCredentialScope: Sendable {
    static let legacyService = "com.proletarius.switch.sms-forwarding"
    enum Failure: Error { case unsafePath, busy, stale }
    let dataRoot: URL
    let runtimeRoot: URL
    let service: String
    let migratesLegacy: Bool
    let lockHome: URL
    private let rootDevice: dev_t
    private let rootInode: ino_t

    @MainActor static func current() throws -> Self {
        var entry = passwd()
        var result: UnsafeMutablePointer<passwd>?
        var buffer = [CChar](repeating: 0, count: 65_536)
        let status = buffer.withUnsafeMutableBufferPointer {
            getpwuid_r(geteuid(), &entry, $0.baseAddress!, $0.count, &result)
        }
        guard status == 0, result != nil, let directory = entry.pw_dir else {
            throw Failure.unsafePath
        }
        // getpwuid_r is independent of HOME, CFFIXED_USER_HOME and SWITCH_DATA_DIR.
        let home = URL(fileURLWithPath: String(cString: directory), isDirectory: true)
        return try Self(dataRoot: SwitchRuntimePaths.dataDirectory(),
                        defaultRoot: home.appendingPathComponent("Library/Application Support/Switch"),
                        lockHome: home)
    }

    init(dataRoot: URL, defaultRoot: URL, lockHome: URL) throws {
        guard dataRoot.isFileURL, defaultRoot.isFileURL, lockHome.isFileURL else {
            throw Failure.unsafePath
        }
        let resolved = dataRoot.standardizedFileURL.resolvingSymlinksInPath()
        let descriptor = open(resolved.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        var bytes = [CChar](repeating: 0, count: Int(PATH_MAX))
        guard bytes.withUnsafeMutableBytes({ fcntl(descriptor, F_GETPATH, $0.baseAddress!) }) == 0 else {
            throw Failure.unsafePath
        }
        let canonical = URL(fileURLWithPath: String(cString: bytes), isDirectory: true)
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              metadata.st_mode & S_IFMT == S_IFDIR,
              metadata.st_uid == geteuid(), metadata.st_mode & 0o022 == 0 else {
            throw Failure.unsafePath
        }
        self.dataRoot = canonical
        runtimeRoot = dataRoot.standardizedFileURL
        rootDevice = metadata.st_dev; rootInode = metadata.st_ino
        service = Self.legacyService + ".root.v2." + Self.digest(canonical.path)
        var defaultMetadata = stat()
        let defaultPath = defaultRoot.standardizedFileURL.resolvingSymlinksInPath().path
        migratesLegacy = lstat(defaultPath, &defaultMetadata) == 0
            && defaultMetadata.st_dev == metadata.st_dev && defaultMetadata.st_ino == metadata.st_ino
        self.lockHome = lockHome.standardizedFileURL
    }

    static func digest(_ value: String) -> String {
        SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    private static func unchangedLock(_ a: stat, _ b: stat) -> Bool {
        a.st_dev == b.st_dev && a.st_ino == b.st_ino && a.st_uid == b.st_uid
            && a.st_mode == b.st_mode && a.st_nlink == b.st_nlink && a.st_size == b.st_size
            && a.st_mtimespec.tv_sec == b.st_mtimespec.tv_sec
            && a.st_mtimespec.tv_nsec == b.st_mtimespec.tv_nsec
            && a.st_ctimespec.tv_sec == b.st_ctimespec.tv_sec
            && a.st_ctimespec.tv_nsec == b.st_ctimespec.tv_nsec
    }

    fileprivate func verifyRoot() throws {
        var metadata = stat()
        guard lstat(dataRoot.path, &metadata) == 0,
              metadata.st_mode & S_IFMT == S_IFDIR,
              metadata.st_uid == geteuid(), metadata.st_mode & 0o022 == 0,
              metadata.st_dev == rootDevice, metadata.st_ino == rootInode else {
            throw Failure.unsafePath
        }
        // APIClient still routes through the configured runtime path. A live
        // alias retarget must not send this pinned root's key to another daemon.
        let descriptor = open(runtimeRoot.path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        guard fstat(descriptor, &metadata) == 0, metadata.st_dev == rootDevice,
              metadata.st_ino == rootInode, metadata.st_uid == geteuid(),
              metadata.st_mode & 0o022 == 0 else { throw Failure.unsafePath }
    }

    func withLock<T>(createIfMissing: Bool = true, _ operation: () throws -> T) throws -> T {
        try withLockProof(createIfMissing: createIfMissing) { _ in try operation() }
    }

    func withLockProof<T>(createIfMissing: Bool = true,
                          _ operation: (() throws -> Void) throws -> T) throws -> T {
        // Open every existing ancestor without following a symlink. Never
        // repair permissions, unlink a lock, or switch to a DataRoot-local lock.
        var parent = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard parent >= 0 else { throw Failure.unsafePath }
        defer { close(parent) }
        for component in lockHome.path.split(separator: "/") {
            let next = openat(parent, String(component), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard next >= 0 else { throw Failure.unsafePath }
            close(parent); parent = next
        }
        var homeInfo = stat()
        guard fstat(parent, &homeInfo) == 0, homeInfo.st_uid == geteuid(),
              homeInfo.st_mode & 0o022 == 0 else { throw Failure.unsafePath }
        let directoryName = ".switch-sms-forwarding-credentials-v2"
        if createIfMissing && mkdirat(parent, directoryName, 0o700) != 0 && errno != EEXIST {
            throw Failure.unsafePath
        }
        let directory = openat(parent, directoryName, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard directory >= 0 else { throw Failure.unsafePath }
        defer { close(directory) }
        var info = stat()
        guard fstat(directory, &info) == 0, info.st_uid == geteuid(),
              info.st_mode & 0o7777 == 0o700 else { throw Failure.unsafePath }
        let directoryIdentity = (device: info.st_dev, inode: info.st_ino)
        let descriptor = openat(directory, "configuration.lock", O_RDWR | (createIfMissing ? O_CREAT : 0) | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0o600)
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        guard fstat(descriptor, &info) == 0, info.st_mode & S_IFMT == S_IFREG,
              info.st_uid == geteuid(), info.st_mode & 0o7777 == 0o600,
              info.st_nlink == 1 else { throw Failure.unsafePath }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            if errno == EWOULDBLOCK || errno == EAGAIN { throw Failure.busy }
            throw Failure.unsafePath
        }
        defer { _ = flock(descriptor, LOCK_UN) }
        func verifyNames() throws {
            // The held parent FD alone cannot prove its absolute namespace:
            // another process could rename lockHome and open a cloned lock.
            var reopened = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC)
            guard reopened >= 0 else { throw Failure.unsafePath }
            defer { close(reopened) }
            for component in lockHome.path.split(separator: "/") {
                let next = openat(reopened, String(component), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                guard next >= 0 else { throw Failure.unsafePath }
                close(reopened); reopened = next
            }
            var currentHome = stat()
            guard fstat(reopened, &currentHome) == 0,
                  currentHome.st_dev == homeInfo.st_dev, currentHome.st_ino == homeInfo.st_ino,
                  currentHome.st_uid == homeInfo.st_uid, currentHome.st_mode == homeInfo.st_mode else {
                throw Failure.unsafePath
            }
            var named = stat()
            guard fstatat(parent, directoryName, &named, AT_SYMLINK_NOFOLLOW) == 0,
                  named.st_dev == directoryIdentity.device, named.st_ino == directoryIdentity.inode,
                  named.st_mode & S_IFMT == S_IFDIR, named.st_uid == geteuid(),
                  named.st_mode & 0o7777 == 0o700,
                  fstatat(directory, "configuration.lock", &named, AT_SYMLINK_NOFOLLOW) == 0,
                  named.st_dev == info.st_dev, named.st_ino == info.st_ino,
                  named.st_mode & S_IFMT == S_IFREG, named.st_uid == geteuid(),
                  named.st_mode & 0o7777 == 0o600, named.st_nlink == 1 else {
                throw Failure.unsafePath
            }
            var held = stat()
            guard fstat(descriptor, &held) == 0,
                  Self.unchangedLock(info, held), Self.unchangedLock(info, named) else { throw Failure.unsafePath }
            try verifyRoot()
        }
        try verifyNames()
        let result = try operation(verifyNames)
        try verifyNames()
        return result
    }

    /// Lock order is credential lock, then queue bootstrap lock. Rust takes
    /// only the latter, then its long-lived queue writer lock.
    func withQueueBootstrapLock<T>(createIfMissing: Bool = true, _ operation: (Int32) throws -> T) throws -> T {
        try withQueueBootstrapLockProof(createIfMissing: createIfMissing) { directory, _ in try operation(directory) }
    }

    func withQueueBootstrapLockProof<T>(createIfMissing: Bool = true,
        _ operation: (Int32, () throws -> Void) throws -> T) throws -> T {
        try verifyRoot()
        let root = open(dataRoot.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard root >= 0 else { throw Failure.unsafePath }
        defer { close(root) }
        var rootInfo = stat()
        guard fstat(root, &rootInfo) == 0, rootInfo.st_dev == rootDevice,
              rootInfo.st_ino == rootInode else { throw Failure.unsafePath }
        if createIfMissing && mkdirat(root, "sms-forwarding", 0o700) != 0 && errno != EEXIST {
            throw Failure.unsafePath
        }
        let directory = openat(root, "sms-forwarding", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard directory >= 0 else { throw Failure.unsafePath }
        defer { close(directory) }
        var directoryInfo = stat()
        guard fstat(directory, &directoryInfo) == 0,
              directoryInfo.st_uid == geteuid(), directoryInfo.st_mode & S_IFMT == S_IFDIR,
              directoryInfo.st_mode & 0o022 == 0,
              (!createIfMissing || fsync(root) == 0) else { throw Failure.unsafePath }
        let descriptor = openat(directory, "state.enc.bootstrap.lock",
                                O_RDWR | (createIfMissing ? O_CREAT : 0) | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0o600)
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        var lockInfo = stat()
        guard fstat(descriptor, &lockInfo) == 0,
              lockInfo.st_uid == geteuid(), lockInfo.st_mode & S_IFMT == S_IFREG,
              lockInfo.st_mode & 0o7777 == 0o600, lockInfo.st_nlink == 1 else {
            throw Failure.unsafePath
        }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            if errno == EWOULDBLOCK || errno == EAGAIN { throw Failure.busy }
            throw Failure.unsafePath
        }
        defer { _ = flock(descriptor, LOCK_UN) }
        func verifyNames() throws {
            var named = stat()
            guard fstatat(root, "sms-forwarding", &named, AT_SYMLINK_NOFOLLOW) == 0,
                  named.st_dev == directoryInfo.st_dev, named.st_ino == directoryInfo.st_ino,
                  named.st_mode == directoryInfo.st_mode, named.st_uid == directoryInfo.st_uid,
                  fstatat(directory, "state.enc.bootstrap.lock", &named, AT_SYMLINK_NOFOLLOW) == 0,
                  named.st_dev == lockInfo.st_dev, named.st_ino == lockInfo.st_ino,
                  named.st_mode & S_IFMT == S_IFREG, named.st_uid == geteuid(),
                  named.st_mode & 0o7777 == 0o600, named.st_nlink == 1 else { throw Failure.unsafePath }
            var held = stat()
            guard fstat(descriptor, &held) == 0,
                  Self.unchangedLock(lockInfo, held), Self.unchangedLock(lockInfo, named) else { throw Failure.unsafePath }
        }
        try verifyNames()
        guard !createIfMissing || fsync(directory) == 0 else { throw Failure.unsafePath }
        let result = try operation(directory, { try verifyNames(); try verifyRoot() })
        try verifyNames()
        try verifyRoot()
        return result
    }

    /// The pre-bootstrap Rust daemon knows only this flock. Keep it through
    /// queue publication and Keychain commit so an in-flight old configure
    /// either owns the lock first or observes the K-prime SWF1 file afterward.
    func withLegacyWriterLock<T>(directory: Int32, createIfMissing: Bool = true, _ operation: () throws -> T) throws -> T {
        try withLegacyWriterLockProof(directory: directory, createIfMissing: createIfMissing) { _ in try operation() }
    }

    func withLegacyWriterLockProof<T>(directory: Int32, createIfMissing: Bool = true,
        _ operation: (() throws -> Void) throws -> T) throws -> T {
        let name = "state.enc.lock"
        let descriptor = openat(directory, name,
                                O_RDWR | (createIfMissing ? O_CREAT : 0) | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0o600)
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        var info = stat()
        guard fstat(descriptor, &info) == 0, info.st_uid == geteuid(),
              info.st_mode & S_IFMT == S_IFREG, info.st_mode & 0o7777 == 0o600,
              info.st_nlink == 1 else { throw Failure.unsafePath }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            if errno == EWOULDBLOCK || errno == EAGAIN { throw Failure.busy }
            throw Failure.unsafePath
        }
        defer { _ = flock(descriptor, LOCK_UN) }
        func verifyName() throws {
            var named = stat()
            guard fstatat(directory, name, &named, AT_SYMLINK_NOFOLLOW) == 0,
                  named.st_dev == info.st_dev, named.st_ino == info.st_ino,
                  named.st_mode & S_IFMT == S_IFREG, named.st_uid == geteuid(),
                  named.st_mode & 0o7777 == 0o600, named.st_nlink == 1 else { throw Failure.unsafePath }
            var held = stat()
            guard fstat(descriptor, &held) == 0,
                  Self.unchangedLock(info, held), Self.unchangedLock(info, named) else { throw Failure.unsafePath }
        }
        try verifyName()
        guard !createIfMissing || fsync(directory) == 0 else { throw Failure.unsafePath }
        let result = try operation(verifyName)
        try verifyName()
        return result
    }

    /// Swift owns credential and flock ordering; Rust owns SWF1 construction,
    /// exclusive publication and durable file/directory sync.
    func publishEmptyQueue(_ encodedKey: String, directory: Int32) throws {
        guard let key = Data(base64Encoded: encodedKey), key.count == 32 else {
            throw Failure.unsafePath
        }
        let status = key.withUnsafeBytes { bytes in
            rustPublishEmptyForwardingQueue(directory,
                bytes.baseAddress?.assumingMemoryBound(to: UInt8.self), bytes.count)
        }
        guard status == 0 else {
            throw NSError(domain: "SwitchForwardingSeed", code: Int(status))
        }
    }

    func checkQueueKeyIdentity(_ encodedKey: String, directory: Int32,
                               createIfMissing: Bool = false) throws {
        guard let rawKey = Data(base64Encoded: encodedKey), rawKey.count == 32 else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 6)
        }
        var digestInput = Data("Switch.SMSForwarding.QueueKey.v1\0".utf8)
        digestInput.append(rawKey)
        var expected = Data("SWFKID1\0".utf8)
        expected.append(contentsOf: SHA256.hash(data: digestInput))
        let name = "state.enc.keyid"
        var descriptor = openat(directory, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        if descriptor < 0 && errno == ENOENT && createIfMissing {
            descriptor = openat(directory, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
            guard descriptor >= 0 else { throw Failure.unsafePath }
            defer { close(descriptor) }
            var info = stat()
            guard fstat(descriptor, &info) == 0, info.st_uid == geteuid(),
                  info.st_mode & S_IFMT == S_IFREG, info.st_nlink == 1,
                  fchmod(descriptor, 0o600) == 0 else { throw Failure.unsafePath }
            let wrote = expected.withUnsafeBytes { bytes in
                write(descriptor, bytes.baseAddress!, bytes.count)
            }
            guard wrote == expected.count, fsync(descriptor) == 0,
                  fsync(directory) == 0 else { throw Failure.unsafePath }
            var named = stat()
            guard fstatat(directory, name, &named, AT_SYMLINK_NOFOLLOW) == 0,
                  named.st_dev == info.st_dev, named.st_ino == info.st_ino,
                  named.st_nlink == 1 else { throw Failure.unsafePath }
            return
        }
        if descriptor < 0 && errno == ENOENT { return }
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        var info = stat(), named = stat()
        guard fstat(descriptor, &info) == 0, info.st_uid == geteuid(),
              info.st_mode & S_IFMT == S_IFREG, info.st_mode & 0o7777 == 0o600,
              info.st_nlink == 1,
              fstatat(directory, name, &named, AT_SYMLINK_NOFOLLOW) == 0,
              named.st_dev == info.st_dev, named.st_ino == info.st_ino else {
            throw Failure.unsafePath
        }
        var bytes = [UInt8](repeating: 0, count: 41)
        let count = read(descriptor, &bytes, bytes.count)
        guard count == expected.count, Data(bytes.prefix(Int(count))) == expected else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 6)
        }
        guard fstatat(directory, name, &named, AT_SYMLINK_NOFOLLOW) == 0,
              named.st_dev == info.st_dev, named.st_ino == info.st_ino,
              named.st_nlink == 1 else { throw Failure.unsafePath }
    }
}

// This value owner never crosses back to MainActor while holding OS locks.
// Swift String/Keychain copies are not claimed to be universally zeroized.
private final class SMSForwardingInitialSetupWorker: @unchecked Sendable {
    enum Mode: Sendable, Equatable { case inspect, complete }
    enum Failure: Sendable { case busy, invalidEvidence, alreadyActive, legacyStateUnsupported, unknown }
    struct Payload: Sendable {
        let account: String
        let stored: SMSForwardingStoredConfiguration
    }
    enum Outcome: Sendable {
        case inspected(missingWitness: Bool)
        case completed(Payload)
        case failed(Failure)
    }
    private struct Candidate {
        let account: String
        let bytes: Data
        let stored: SMSForwardingStoredConfiguration
    }
    private struct Refusal: Error { let kind: Failure }
    static let queue = DispatchQueue(label: "Switch.forwarding.initial-setup-recovery")
    private let scope: SMSForwardingCredentialScope
    private let active = "configuration.active.v1"
    private let prefix = "configuration.snapshot.v1."
    init(scope: SMSForwardingCredentialScope) { self.scope = scope }

    private func accounts(service: String? = nil) throws -> [String] {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service ?? scope.service, kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return [] }
        guard status == errSecSuccess, let attributes = result as? [[String: Any]],
              attributes.count <= 17,
              attributes.allSatisfy({ $0[kSecAttrAccount as String] is String }) else {
            throw Refusal(kind: .invalidEvidence)
        }
        let names = attributes.compactMap { $0[kSecAttrAccount as String] as? String }
        guard Set(names).count == names.count else { throw Refusal(kind: .invalidEvidence) }
        let targetService = service ?? scope.service
        if targetService == SMSForwardingCredentialScope.legacyService {
            // This bounded first-setup transaction does not migrate any legacy
            // account, including otherwise recognized pre-v1 provider records.
            // Existing startup migration is separate and remains unchanged.
            guard names.isEmpty else { throw Refusal(kind: .legacyStateUnsupported) }
        } else {
            // Do not silently filter future/unknown evidence out of the proof.
            // Active is verified absent before add and exact after readback.
            guard targetService == scope.service,
                  names.allSatisfy({ $0 == active || $0.hasPrefix(prefix) }) else {
                throw Refusal(kind: .invalidEvidence)
            }
        }
        let stages = names.filter { $0.hasPrefix(prefix) }
        guard stages.allSatisfy({ UUID(uuidString: String($0.dropFirst(prefix.count))) != nil }) else {
            throw Refusal(kind: .invalidEvidence)
        }
        return stages.sorted()
    }
    private func selection(expectedActive: String? = nil) throws -> Candidate {
        guard try SMSForwardingStore.readSecret(active, service: scope.service) == expectedActive else {
            throw Refusal(kind: .alreadyActive)
        }
        if scope.migratesLegacy {
            guard try SMSForwardingStore.readSecret(active, service: SMSForwardingCredentialScope.legacyService) == nil,
                  try SMSForwardingStore.readSecret("state.encryptionKey", service: SMSForwardingCredentialScope.legacyService) == nil,
                  try accounts(service: SMSForwardingCredentialScope.legacyService).isEmpty else {
                throw Refusal(kind: .legacyStateUnsupported)
            }
        }
        guard try SMSForwardingStore.readSecret("state.encryptionKey", service: scope.service) == nil else {
            throw Refusal(kind: .invalidEvidence)
        }
        let names = try accounts()
        guard names.count == 1,
              let data = try SMSForwardingStore.readSecretBytes(names[0], service: scope.service),
              let stored = try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: data),
              stored.schemaVersion == 1, stored.intent.clientSequence > 0,
              !stored.intent.clientRequestID.isEmpty,
              Data(base64Encoded: stored.configuration.stateKey)?.count == 32 else {
            throw Refusal(kind: .invalidEvidence)
        }
        return Candidate(account: names[0], bytes: data, stored: stored)
    }
    private func recheck(_ original: Candidate, expectedActive: String? = nil) throws {
        let current = try selection(expectedActive: expectedActive)
        guard current.account == original.account, current.bytes == original.bytes else {
            throw Refusal(kind: .invalidEvidence)
        }
    }
    func run(_ mode: Mode) -> Outcome {
        do {
            return try scope.withLockProof(createIfMissing: false) { credentialProof in
              try scope.withQueueBootstrapLockProof(createIfMissing: false) { directory, bootstrapProof in
                try scope.withLegacyWriterLockProof(directory: directory, createIfMissing: false) { writerProof in
                    func namespaces() throws {
                        try credentialProof(); try bootstrapProof(); try writerProof()
                    }
                    let original = try selection()
                    guard var key = Data(base64Encoded: original.stored.configuration.stateKey), key.count == 32 else {
                        throw Refusal(kind: .invalidEvidence)
                    }
                    defer { key.resetBytes(in: 0..<key.count) }
                    if mode == .inspect {
                        var observation: UnsafeMutableRawPointer?
                        defer { rustDestroyForwardingDiagnosis(observation) }
                        var result: UInt8 = 0
                        let status = key.withUnsafeBytes { bytes in
                            rustDiagnoseForwardingQueue(directory, bytes.baseAddress!.assumingMemoryBound(to: UInt8.self),
                                                        1, &result, &observation)
                        }
                        guard status >= 0, result & 15 == 1, result >> 4 == 0 || result >> 4 == 1 else {
                            throw Refusal(kind: .invalidEvidence)
                        }
                        try recheck(original); try namespaces()
                        guard rustConfirmForwardingDiagnosis(observation) == 0 else { throw Refusal(kind: .invalidEvidence) }
                        return .inspected(missingWitness: result >> 4 == 0)
                    }
                    var proof: UnsafeMutableRawPointer?
                    defer { rustDestroyForwardingRecovery(proof) }
                    let begun = key.withUnsafeBytes { bytes in
                        rustBeginForwardingRecovery(directory, bytes.baseAddress!.assumingMemoryBound(to: UInt8.self), key.count, &proof)
                    }
                    guard begun == 0, proof != nil else { throw Refusal(kind: .invalidEvidence) }
                    #if SWITCH_FORWARDING_TESTS
                    ForwardingKeychain.barrier("recovery-captured")
                    #endif
                    try recheck(original); try namespaces()
                    guard rustPrepareForwardingRecoveryWitness(proof) == 0 else { throw Refusal(kind: .unknown) }
                    #if SWITCH_FORWARDING_TESTS
                    ForwardingKeychain.barrier("recovery-witness")
                    #endif
                    try recheck(original); try namespaces()
                    guard rustConfirmForwardingRecovery(proof) == 0 else { throw Refusal(kind: .invalidEvidence) }
                    #if SWITCH_FORWARDING_TESTS
                    ForwardingKeychain.barrier("recovery-before-pointer")
                    #endif
                    // The barrier can model a race at the very last boundary.
                    try recheck(original); try namespaces()
                    guard rustConfirmForwardingRecovery(proof) == 0 else { throw Refusal(kind: .invalidEvidence) }
                    try SMSForwardingStore.addAndVerifySecret(original.account, name: active, service: scope.service)
                    #if SWITCH_FORWARDING_TESTS
                    ForwardingKeychain.barrier("recovery-pointer-readback")
                    #endif
                    try recheck(original, expectedActive: original.account); try namespaces()
                    guard rustConfirmForwardingRecovery(proof) == 0 else { throw Refusal(kind: .unknown) }
                    return .completed(Payload(account: original.account, stored: original.stored))
                }
              }
            }
        } catch SMSForwardingCredentialScope.Failure.busy { return .failed(.busy) }
        catch let refusal as Refusal { return .failed(refusal.kind) }
        catch { return .failed(.unknown) }
    }
}

@MainActor
final class SMSForwardingStore: ObservableObject {
    static let shared = SMSForwardingStore()

    @Published var telegramEnabled = false
    @Published var telegramBotToken = ""
    @Published var telegramChatID = ""
    @Published var telegramProxyMode = "system"
    @Published var telegramProxyURL = ""
    @Published var barkEnabled = false
    @Published var barkServerURL = "https://api.day.app"
    @Published var barkDeviceKey = ""
    @Published var barkIconURL = ""
    @Published var barkGroup = "Switch-SMS"
    @Published var barkEncrypted = false
    @Published var barkEncryptionAlgorithm = "AES-128-CBC"
    @Published var barkEncryptionKey = ""
    @Published private(set) var isSaving = false
    @Published private(set) var isTestingTelegram = false
    @Published private(set) var isTestingBark = false
    @Published private(set) var status: SMSForwardingStatus?
    @Published private(set) var errorMessage: String?
    @Published private(set) var telegramTestMessage: String?
    @Published private(set) var barkTestMessage: String?
    @Published private(set) var resolvingTaskID: String?
    @Published private(set) var isCredentialRecoveryInProgress = false
    @Published private(set) var credentialRecoveryMessage: String?
    @Published private(set) var canAttemptInitialSetupCompletion = false
    var needsInitialSetupRecovery: Bool { !credentialScopeLoaded && credentialScope != nil }

    @Published private var pendingTests: [String: SMSForwardingPendingTest] = [:]
    private var testCheckpointFailure = false
    private let testCheckpoint: SMSForwardingTestCheckpoint
    private var testingProviders = Set<String>()
    private let defaults = UserDefaults.standard
    private let checkpoint: SMSForwardingCheckpoint
    private let handoffAuthority: SMSForwardingRustAuthority
    private var cancellables = Set<AnyCancellable>()
    private var isRefreshingStatus = false
    // Presentation generation only: it suppresses a stale response from
    // overwriting newer form feedback. It never retries or admits work.
    private var uiSnapshotGeneration: UInt64 = 0
    private var confirmedConfigurationID: String?
    private var configurationIntent = SMSForwardingConfigurationIntent(
        clientRequestID: "", clientSequence: 0)
    private var stateEncryptionKey = ""
    // OS credential availability only; provider/queue decisions remain Rust-owned.
    // A failed initial read stays fenced until reopen, preserving form edits.
    private var credentialLoadFailure: String?
    private var appliedConfiguration: SMSForwardingConfiguration?
    private var credentialScope: SMSForwardingCredentialScope?
    private var loadedActiveAccount: String?
    private var credentialScopeLoaded = false
    private var initialSetupRecoveryUnconfirmed = false
    private var admissionHandoffInFlight = false
    private var admissionHandoffSubmitted = false
    private static let frozenHandoffSnapshotKey = "switch.smsForwarding.rustHandoffSnapshotV1"

    private enum Key {
        static let activeConfigurationAccount = "configuration.active.v1"
        static let snapshotAccountPrefix = "configuration.snapshot.v1."
        static let telegramEnabled = "switch.smsForwarding.telegram.enabled"
        static let telegramChatID = "switch.smsForwarding.telegram.chatID"
        static let telegramProxyMode = "switch.smsForwarding.telegram.proxyMode"
        static let barkEnabled = "switch.smsForwarding.bark.enabled"
        static let barkServerURL = "switch.smsForwarding.bark.serverURL"
        static let barkIconURL = "switch.smsForwarding.bark.iconURL"
        static let barkGroup = "switch.smsForwarding.bark.group"
        static let barkEncrypted = "switch.smsForwarding.bark.encrypted"
        static let barkEncryptionAlgorithm = "switch.smsForwarding.bark.algorithm"
        static let credentialsUpdated = "switch.smsForwarding.credentialsUpdated"
        static let configurationIntentID = "switch.smsForwarding.configurationIntent.id"
        static let configurationIntentSequence = "switch.smsForwarding.configurationIntent.sequence"
    }

    private init(checkpoint: SMSForwardingCheckpoint? = nil) {
        if let checkpoint {
            self.checkpoint = checkpoint
        } else {
            // BackendProcess establishes the shared private data directory.
            // Pin it after creation, including on the first application run.
            _ = BackendProcess.shared
            self.checkpoint = SMSForwardingCheckpoint(directory: SwitchRuntimePaths.dataDirectory())
        }
        handoffAuthority = SMSForwardingRustAuthority(checkpoint: self.checkpoint)
        testCheckpoint = SMSForwardingTestCheckpoint(directory: SwitchRuntimePaths.dataDirectory())
        do {
            pendingTests = try testCheckpoint.load()
            if pendingTests["telegram"] != nil {
                telegramTestMessage = "上次测试通知结果待确认，可查询原回执。"
            }
            if pendingTests["bark"] != nil {
                barkTestMessage = "上次测试通知结果待确认，可查询原回执。"
            }
        } catch { testCheckpointFailure = true }
        do {
            let scope = try SMSForwardingCredentialScope.current()
            credentialScope = scope
            try scope.withLock {
              try scope.withQueueBootstrapLock { queueDirectory in
                let scopedActive = try Self.readSecret(Key.activeConfigurationAccount, service: scope.service)
                let legacyActive = try (scopedActive == nil && scope.migratesLegacy)
                    ? Self.readSecret(Key.activeConfigurationAccount, service: SMSForwardingCredentialScope.legacyService) : nil
                if let activeAccount = scopedActive ?? legacyActive {
                    let service = scopedActive == nil ? SMSForwardingCredentialScope.legacyService : scope.service
                    guard activeAccount.hasPrefix(Key.snapshotAccountPrefix),
                          UUID(uuidString: String(activeAccount.dropFirst(Key.snapshotAccountPrefix.count))) != nil,
                          let encoded = try Self.readSecret(activeAccount, service: service),
                          let data = encoded.data(using: .utf8),
                          let stored = try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: data),
                          stored.schemaVersion == 1,
                          !stored.intent.clientRequestID.isEmpty,
                          stored.intent.clientSequence > 0,
                          Data(base64Encoded: stored.configuration.stateKey)?.count == 32 else {
                        throw NSError(domain: "SwitchForwardingCredentials", code: 2)
                    }
                    try scope.checkQueueKeyIdentity(stored.configuration.stateKey, directory: queueDirectory)
                    applyStoredConfiguration(stored.configuration)
                    configurationIntent = stored.intent
                    if scopedActive == nil {
                        try commitConfigurationLocked(stored.configuration, intent: stored.intent, scope: scope)
                    } else { loadedActiveAccount = activeAccount }
                    appliedConfiguration = stored.configuration
                    if scopedActive != nil {
                        reclaimSnapshotsLocked(activeAccount: activeAccount, encoded: encoded, scope: scope)
                    }
                } else {
                    // Pre-v1 accounts remain read only. Bootstrap commits one complete
                    // snapshot before backend-running is allowed to publish it.
                    if scope.migratesLegacy {
                        telegramEnabled = defaults.bool(forKey: Key.telegramEnabled)
                        telegramChatID = defaults.string(forKey: Key.telegramChatID) ?? ""
                        telegramProxyMode = defaults.string(forKey: Key.telegramProxyMode) ?? "system"
                        barkEnabled = defaults.bool(forKey: Key.barkEnabled)
                        barkServerURL = defaults.string(forKey: Key.barkServerURL) ?? "https://api.day.app"
                        barkIconURL = defaults.string(forKey: Key.barkIconURL) ?? ""
                        barkGroup = defaults.string(forKey: Key.barkGroup) ?? "Switch-SMS"
                        barkEncrypted = defaults.bool(forKey: Key.barkEncrypted)
                        barkEncryptionAlgorithm = defaults.string(forKey: Key.barkEncryptionAlgorithm) ?? "AES-128-CBC"
                    }
                    // Read the entire OS snapshot before publishing any values or
                    // generating a key. Only confirmed item-not-found means absent.
                    let token = try scope.migratesLegacy ? Self.readSecret("telegram.botToken", service: SMSForwardingCredentialScope.legacyService) : nil
                    let proxy = try scope.migratesLegacy ? Self.readSecret("telegram.proxyURL", service: SMSForwardingCredentialScope.legacyService) : nil
                    let deviceKey = try scope.migratesLegacy ? Self.readSecret("bark.deviceKey", service: SMSForwardingCredentialScope.legacyService) : nil
                    let encryptionKey = try scope.migratesLegacy ? Self.readSecret("bark.encryptionKey", service: SMSForwardingCredentialScope.legacyService) : nil
                    let queueKey = try scope.migratesLegacy ? Self.readSecret("state.encryptionKey", service: SMSForwardingCredentialScope.legacyService) : nil
                    telegramBotToken = token ?? ""
                    telegramProxyURL = proxy ?? ""
                    barkDeviceKey = deviceKey ?? ""
                    barkEncryptionKey = encryptionKey ?? ""
                    @MainActor func commitBootstrap(key: String) throws {
                        let configuration = makeConfiguration(stateKey: key)
                        let intent = try scope.migratesLegacy
                            ? (queueKey == nil ? advancedConfigurationIntent(legacySequence: legacyConfigurationSequence()) : restoredConfigurationIntent())
                            : SMSForwardingConfigurationIntent(clientRequestID: UUID().uuidString.lowercased(), clientSequence: 1)
                        try commitConfigurationLocked(configuration, intent: intent, scope: scope)
                        stateEncryptionKey = key
                        configurationIntent = intent
                        appliedConfiguration = configuration
                    }
                    if let queueKey {
                        guard Data(base64Encoded: queueKey)?.count == 32 else {
                            throw NSError(domain: "SwitchForwardingCredentials", code: 6)
                        }
                        try scope.checkQueueKeyIdentity(queueKey, directory: queueDirectory)
                        try commitBootstrap(key: queueKey)
                    } else {
                        try scope.withLegacyWriterLock(directory: queueDirectory) {
                            // Old Rust ignores bootstrap and witness, but must
                            // take this historical lock before opening the queue.
                            try Self.confirmQueueAbsentBeforeGeneratingKey(directory: queueDirectory)
                            // A prior stage is evidence of an interrupted bootstrap.
                            // Never infer its authority from account order or create
                            // another candidate before the queue is diagnosed.
                            guard try snapshotAccountsLocked(scope: scope).isEmpty else {
                                throw NSError(domain: "SwitchForwardingCredentials", code: 10)
                            }
                            let newKey = try Self.generateStateEncryptionKey()
                            let configuration = makeConfiguration(stateKey: newKey)
                            let intent = try scope.migratesLegacy
                                ? advancedConfigurationIntent(legacySequence: legacyConfigurationSequence())
                                : SMSForwardingConfigurationIntent(clientRequestID: UUID().uuidString.lowercased(), clientSequence: 1)
                            let stage = try stageConfigurationLocked(configuration, intent: intent, scope: scope)
                            #if SWITCH_FORWARDING_TESTS
                            ForwardingKeychain.barrier("stage-verified")
                            ForwardingKeychain.barrier("before-seed")
                            #endif
                            try scope.publishEmptyQueue(newKey, directory: queueDirectory)
                            #if SWITCH_FORWARDING_TESTS
                            ForwardingKeychain.barrier("queue")
                            #endif
                            try scope.checkQueueKeyIdentity(newKey, directory: queueDirectory,
                                                            createIfMissing: true)
                            #if SWITCH_FORWARDING_TESTS
                            ForwardingKeychain.barrier("witness")
                            #endif
                            try activateStagedConfigurationLocked(stage, scope: scope, expectedActiveAccount: nil)
                            stateEncryptionKey = newKey
                            configurationIntent = intent
                            appliedConfiguration = configuration
                        }
                    }
                }
                credentialScopeLoaded = true
              }
            }
        } catch SMSForwardingCredentialScope.Failure.busy {
            errorMessage = "其他 Switch 实例正在提交转发配置。请稍后重新启动 Switch；当前未读取或提交配置。"
        } catch {
            let failure = error as NSError
            let message: String
            if failure.domain == "SwitchForwardingCredentials" && failure.code == 6 {
                message = "现有转发队列或配置缺少可用原密钥。请恢复原钥匙串凭据后重新启动 Switch；不会覆盖现有队列。"
            } else if failure.domain == "SwitchForwardingCredentials" && failure.code == 10
                        || failure.domain == "SwitchForwardingSeed" {
                message = "转发队列初始化未完成，已保留阶段凭据或队列文件。请先诊断并恢复原状态；不会自动换钥或删除数据。"
            } else {
                message = "钥匙串凭据读取失败（\(failure.code)）。请解锁登录钥匙串后重新启动 Switch；恢复前不会保存、覆盖或删除凭据。"
            }
            credentialLoadFailure = message
            errorMessage = message
        }

        BackendProcess.shared.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                guard let self else { return }
                guard state == .running else {
                    self.confirmedConfigurationID = nil
                    return
                }
                Task { @MainActor [weak self] in
                    await self?.synchronizeAppliedConfiguration()
                }
            }
            .store(in: &cancellables)
    }

    private func synchronizeAppliedConfiguration() async {
        if let credentialLoadFailure {
            errorMessage = credentialLoadFailure
            return
        }
        guard credentialScopeLoaded, let configuration = appliedConfiguration else { return }
        do { try verifyCurrentCredentialSelection() }
        catch {
            errorMessage = credentialCoordinationMessage(error)
            return
        }
        let intent = configurationIntent
        let generation = uiSnapshotGeneration
        let result = await synchronizeConfiguration(configuration, intent: intent,
                                                    presentationGeneration: generation)
        presentConfigurationSync(result, presentationGeneration: generation)
    }

    var isEnabled: Bool {
        appliedConfiguration?.telegram.enabled == true || appliedConfiguration?.bark.enabled == true
    }

    func save() async {
        guard !isSaving else { return }
        guard credentialScopeLoaded else {
            errorMessage = "转发配置尚未安全读取，请稍后重新启动 Switch。"
            return
        }
        if let credentialLoadFailure {
            errorMessage = credentialLoadFailure
            return
        }
        errorMessage = nil
        let nextIntent: SMSForwardingConfigurationIntent
        do { nextIntent = try persist() }
        catch {
            errorMessage = error.localizedDescription
            return
        }
        appliedConfiguration = makeConfiguration()
        uiSnapshotGeneration &+= 1
        configurationIntent = nextIntent
        confirmedConfigurationID = nil
        isSaving = true
        defer { isSaving = false }
        if let configuration = appliedConfiguration {
            let generation = uiSnapshotGeneration
            let result = await synchronizeConfiguration(configuration,
                intent: configurationIntent, presentationGeneration: generation)
            presentConfigurationSync(result, presentationGeneration: generation)
        }
    }

    func refreshStatus() async {
        guard !isRefreshingStatus, BackendProcess.shared.state == .running else { return }
        isRefreshingStatus = true
        defer { isRefreshingStatus = false }
        do {
            try verifyRuntimeRoot()
            let response: SMSForwardingStatus = try await APIClient().get("api/sms/forwarding/status")
            try verifyRuntimeRoot()
            guard !Task.isCancelled else { return }
            status = response
            await submitCurrentAdmissionHandoff()
        } catch {
            guard !Task.isCancelled else { return }
            errorMessage = userFacingErrorMessage(error)
        }
    }

    func resolveTask(id: String, provider: String, action: String,
                     expectedConfigurationID: String, expectedAttempts: Int,
                     expectedState: String, expectedDestinationRevision: String) async {
        guard resolvingTaskID == nil else { return }
        resolvingTaskID = id
        defer { resolvingTaskID = nil }
        do {
            try verifyRuntimeRoot()
            let _: SMSForwardingQueueActionResponse = try await APIClient().send(
                "api/sms/forwarding/queue-action",
                body: SMSForwardingQueueActionRequest(
                    id: id, provider: provider, action: action,
                    expectedConfigurationID: expectedConfigurationID,
                    expectedAttempts: expectedAttempts,
                    expectedState: expectedState,
                    expectedDestinationRevision: expectedDestinationRevision))
            try verifyRuntimeRoot()
            errorMessage = nil
            await refreshStatus()
        } catch { errorMessage = userFacingErrorMessage(error) }
    }

    /// Imports the legacy Swift admission checkpoint once. Rust owns both
    /// current message producers and durable forwarding admission; this UI
    /// bridge freezes legacy progress and never enqueues messages itself.
    func submitAdmissionHandoff(
        deviceAcceptedNotificationIDs: [String],
        deviceWasEnabled: Bool?,
        nwaitCursor: String?,
        nwaitSeeded: Bool,
        nwaitWasEnabled: Bool?
    ) async throws -> SMSForwardingHandoffAck {
        guard !admissionHandoffInFlight else { throw SMSForwardingHandoffError.handoffInProgress }
        admissionHandoffInFlight = true
        defer { admissionHandoffInFlight = false }
        try verifyRuntimeRoot()
        let snapshot = try await frozenAdmissionHandoff(
            deviceAcceptedNotificationIDs: deviceAcceptedNotificationIDs,
            deviceWasEnabled: deviceWasEnabled,
            nwaitCursor: nwaitCursor,
            nwaitSeeded: nwaitSeeded,
            nwaitWasEnabled: nwaitWasEnabled)
        try verifyRuntimeRoot()
        let acknowledgement: SMSForwardingHandoffAck = try await APIClient().send(
            "api/v2/sms/forwarding/handoff", body: snapshot, as: SMSForwardingHandoffAck.self)
        try verifyRuntimeRoot()
        guard acknowledgement.accepted, acknowledgement.handoffID == snapshot.handoffID else {
            throw SMSForwardingHandoffError.acknowledgementMismatch
        }
        return acknowledgement
    }

    func submitAdmissionHandoff(
        deviceSMS: DeviceSMSForwardingHandoff,
        nwaitSMS: NwaitSMSForwardingHandoff
    ) async throws -> SMSForwardingHandoffAck {
        try await submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: deviceSMS.acceptedNotificationIDs,
            deviceWasEnabled: deviceSMS.wasEnabled,
            nwaitCursor: nwaitSMS.cursor,
            nwaitSeeded: nwaitSMS.seeded,
            nwaitWasEnabled: nwaitSMS.wasEnabled)
    }

    private func frozenAdmissionHandoff(
        deviceAcceptedNotificationIDs: [String],
        deviceWasEnabled: Bool?,
        nwaitCursor: String?,
        nwaitSeeded: Bool,
        nwaitWasEnabled: Bool?
    ) async throws -> SMSForwardingHandoffV1 {
        do {
            try verifyRuntimeRoot()
            if let bytes = try handoffAuthority.local(legacy: defaults.data(forKey: Self.frozenHandoffSnapshotKey)) {
                return try Self.decodeFrozenHandoff(bytes)
            }
            let current: SMSForwardingHandoffSnapshotResponse = try await APIClient().get(
                "api/v2/sms/forwarding/handoff", as: SMSForwardingHandoffSnapshotResponse.self)
            try verifyRuntimeRoot()
            let source = SMSForwardingLegacyProgress(
                deviceSMS: DeviceSMSForwardingHandoff(acceptedNotificationIDs: deviceAcceptedNotificationIDs,
                    wasEnabled: deviceWasEnabled),
                nwaitSMS: NwaitSMSForwardingHandoff(cursor: nwaitCursor, seeded: nwaitSeeded,
                    wasEnabled: nwaitWasEnabled))
            let snapshot = try current.snapshot.map { try JSONEncoder().encode($0) }
            let confirmed = try handoffAuthority.remote(accepted: current.accepted, snapshot: snapshot,
                source: current.accepted ? Data() : JSONEncoder().encode(source))
            return try Self.decodeFrozenHandoff(confirmed)
        } catch let failure as SMSForwardingRustFailure {
            switch failure.code {
            case -10: throw SMSForwardingHandoffError.invalidFrozenSnapshot
            case -11: throw SMSForwardingHandoffError.tooManyAcceptedIDs
            case -12: throw SMSForwardingHandoffError.invalidAcceptedID
            case -13: throw SMSForwardingHandoffError.cursorTooLong
            default: throw SMSForwardingHandoffError.snapshotPersistenceFailed
            }
        }
    }

    private static func decodeFrozenHandoff(_ bytes: Data) throws -> SMSForwardingHandoffV1 {
        do {
            return try JSONDecoder().decode(SMSForwardingHandoffV1.self,
                from: SMSForwardingRustAuthority.validated(bytes))
        } catch {
            throw SMSForwardingHandoffError.invalidFrozenSnapshot
        }
    }

    func hasUnresolvedTest(_ provider: String) -> Bool {
        pendingTests[provider] != nil || testCheckpointFailure
    }

    func canStartNewTestAfterUnknown(_ provider: String) -> Bool {
        pendingTests[provider] != nil && !testCheckpointFailure
    }

    func test(_ provider: String) async {
        await runTest(provider, replaceConfirmedUnknown: false)
    }

    /// A new test is a distinct user-authorized dispatch. The original test
    /// is first queried with its durable ID and must have an explicit Rust
    /// unknown-no-replay receipt; transport ambiguity never permits bypassing
    /// the original admission fence.
    func startNewTestAfterUnknown(_ provider: String) async {
        await runTest(provider, replaceConfirmedUnknown: true)
    }

    private func runTest(_ provider: String, replaceConfirmedUnknown: Bool) async {
        guard ["telegram", "bark"].contains(provider),
              !testingProviders.contains(provider) else { return }
        testingProviders.insert(provider)
        if provider == "telegram" { isTestingTelegram = true }
        else { isTestingBark = true }
        defer {
            testingProviders.remove(provider)
            if provider == "telegram" { isTestingTelegram = false }
            else { isTestingBark = false }
        }
        guard !testCheckpointFailure else {
            presentTestMessage("测试请求记录不可用，请恢复记录后查询原请求。", provider: provider)
            return
        }
        do { try verifyRuntimeRoot() }
        catch { presentTestMessage(credentialCoordinationMessage(error), provider: provider); return }
        // An unresolved request is an admission fence, including after GUI
        // reopen/config edits. Retrying submits its ORIGINAL payload and ID.
        if let pending = pendingTests[provider] {
            guard replaceConfirmedUnknown else {
                await submitTest(pending, generation: uiSnapshotGeneration)
                return
            }
            do {
                let receipt: SMSForwardingTestResponse = try await APIClient().send(
                    "api/sms/forwarding/test", body: pending.request, as: SMSForwardingTestResponse.self)
                try verifyRuntimeRoot()
                guard receipt.clientRequestID == pending.request.clientRequestID,
                      receipt.provider == pending.request.provider,
                      receipt.expectedConfigurationID == pending.request.expectedConfigurationID else {
                    presentTestMessage("原测试回执不匹配，未发起新测试。", provider: provider)
                    return
                }
                if receipt.terminal && ["succeeded", "failed"].contains(receipt.state) {
                    await submitTest(pending, generation: uiSnapshotGeneration)
                    return
                }
                guard receipt.state == "unknown_no_replay", !receipt.terminal, !receipt.sent,
                      receipt.dispatchCount == 1 else {
                    presentTestMessage("原测试尚未确认结果，未发起新测试。请稍后查询回执。", provider: provider)
                    return
                }
            } catch {
                presentTestMessage("无法确认原测试结果；为避免重复通知，未发起新测试。请稍后查询原回执。", provider: provider)
                return
            }
        } else if replaceConfirmedUnknown {
            presentTestMessage("没有待确认的原测试，请使用“测试”按钮。", provider: provider)
            return
        }
        if let credentialLoadFailure {
            presentTestMessage(credentialLoadFailure, provider: provider)
            return
        }
        errorMessage = nil
        let nextIntent: SMSForwardingConfigurationIntent
        do { nextIntent = try persist() }
        catch { presentTestMessage(error.localizedDescription, provider: provider); return }
        appliedConfiguration = makeConfiguration()
        uiSnapshotGeneration &+= 1
        configurationIntent = nextIntent
        let generation = uiSnapshotGeneration
        let intent = configurationIntent
        confirmedConfigurationID = nil
        let result: ConfigurationSyncResult
        if let configuration = appliedConfiguration {
            result = await synchronizeConfiguration(configuration, intent: intent,
                presentationGeneration: generation)
        } else { result = .failed("本机转发配置不可用。") }
        let expectedConfigurationID: String
        switch result {
        case let .confirmed(configurationID):
            guard generation == uiSnapshotGeneration, intent == configurationIntent,
                  let configurationID, !configurationID.isEmpty else { return }
            expectedConfigurationID = configurationID
        case .stale: return
        case let .failed(message):
            guard generation == uiSnapshotGeneration, intent == configurationIntent else { return }
            presentTestMessage(message, provider: provider)
            return
        }
        let pending = SMSForwardingPendingTest(request: SMSForwardingProviderRequest(
            clientRequestID: UUID().uuidString.lowercased(), provider: provider,
            expectedConfigurationID: expectedConfigurationID), configurationIntentID: intent.clientRequestID)
        var next = pendingTests
        next[provider] = pending
        do {
            try testCheckpoint.save(next)
            pendingTests = next
        } catch {
            // A failed fsync may already have renamed the file. Fence this
            // instance until reopen loads the original bytes; never dispatch.
            testCheckpointFailure = true
            presentTestMessage("测试请求记录保存失败，未提交测试通知。", provider: provider)
            return
        }
        await submitTest(pending, generation: generation,
                         duplicateRisk: replaceConfirmedUnknown)
    }

    private func submitTest(_ pending: SMSForwardingPendingTest, generation: UInt64,
                            duplicateRisk: Bool = false) async {
        let request = pending.request
        // An unavailable Keychain leaves no applied intent. A validated receipt
        // for an already retained request may still update its own presentation.
        let blockedCredentialPresentation = credentialLoadFailure != nil
            && appliedConfiguration == nil && configurationIntent.clientRequestID.isEmpty
        do {
            try verifyRuntimeRoot()
            let receipt: SMSForwardingTestResponse = try await APIClient().send(
                "api/sms/forwarding/test", body: request, as: SMSForwardingTestResponse.self)
            try verifyRuntimeRoot()
            guard receipt.clientRequestID == request.clientRequestID,
                  receipt.provider == request.provider,
                  receipt.expectedConfigurationID == request.expectedConfigurationID,
                  (0...1).contains(receipt.dispatchCount) else {
                throw SMSForwardingTestCheckpoint.Failure.invalid
            }
            let knownSuccess = receipt.state == "succeeded" && receipt.sent
                && receipt.terminal && receipt.dispatchCount == 1
            let knownFailure = receipt.state == "failed" && !receipt.sent && receipt.terminal
            if knownSuccess || knownFailure {
                // Clearing an OLD fence is safe, but never clear a different
                // request or overwrite a newer configuration's presentation.
                if pendingTests[request.provider] == pending {
                    var next = pendingTests
                    next.removeValue(forKey: request.provider)
                    try testCheckpoint.save(next)
                    pendingTests = next
                }
            }
            guard generation == uiSnapshotGeneration,
                  blockedCredentialPresentation || pending.configurationIntentID == configurationIntent.clientRequestID else { return }
            let message: String
            if knownSuccess {
                message = "测试通知已发送"
            } else if knownFailure {
                message = "测试通知未发送：\(SMSForwardingFailurePresentation.message(receipt.code, provider: request.provider))"
            } else {
                let reason = receipt.state == "unknown_no_replay"
                    && receipt.code != "sms_forwarding_test_result_unknown"
                    ? SMSForwardingFailurePresentation.message(receipt.code, provider: request.provider) + " "
                    : ""
                message = reason + "测试通知结果未知，已保留原请求；再次操作仅查询原回执。"
            }
            presentTestMessage(message + (duplicateRisk ? " 原测试结果仍未知，接收端可能出现重复通知。" : ""),
                               provider: request.provider)
        } catch {
            guard generation == uiSnapshotGeneration,
                  blockedCredentialPresentation || pending.configurationIntentID == configurationIntent.clientRequestID else { return }
            presentTestMessage("测试通知结果未知，已保留原请求；再次操作仅查询原回执。", provider: request.provider)
        }
    }

    private func presentTestMessage(_ message: String, provider: String) {
        if provider == "telegram" { telegramTestMessage = message }
        else { barkTestMessage = message }
    }

    private func credentialCoordinationMessage(_ error: Error) -> String {
        if case SMSForwardingCredentialScope.Failure.busy = error {
            return "其他 Switch 实例正在提交转发配置。请稍后重试；原配置保持不变。"
        }
        return "转发配置归属或版本已变化。请重新启动 Switch 后再保存；未覆盖现有配置。"
    }

    /// Runtime routing must stay on the captured directory even while the
    /// Keychain is unavailable and an original request receipt is queried.
    private func verifyRuntimeRoot() throws {
        guard let scope = credentialScope else {
            throw SMSForwardingCredentialScope.Failure.unsafePath
        }
        try scope.verifyRoot()
    }

    func inspectInitialSetupRecovery() async {
        await runInitialSetupRecovery(.inspect)
    }

    /// Only a fresh explicit click enters this transaction. Diagnosis is never
    /// a grant, and neither startup nor backend lifecycle invokes this method.
    func completeInterruptedInitialSetup() async {
        await runInitialSetupRecovery(.complete)
    }

    private func runInitialSetupRecovery(_ mode: SMSForwardingInitialSetupWorker.Mode) async {
        guard !isCredentialRecoveryInProgress, !isSaving, let scope = credentialScope else { return }
        if mode == .complete && initialSetupRecoveryUnconfirmed {
            credentialRecoveryMessage = "上次完成结果尚未确认，请重新启动 Switch 后检查。当前不会重试覆盖。"
            return
        }
        isCredentialRecoveryInProgress = true
        canAttemptInitialSetupCompletion = false
        defer { isCredentialRecoveryInProgress = false }
        let worker = SMSForwardingInitialSetupWorker(scope: scope)
        // No cancellation handler unlocks or kills this worker. The continuation
        // returns only after the actual SecItem operation/result and all locks
        // retire, even if the sheet's observing Task was cancelled meanwhile.
        let result = await withCheckedContinuation { continuation in
            SMSForwardingInitialSetupWorker.queue.async {
                continuation.resume(returning: worker.run(mode))
            }
        }
        switch result {
        case .inspected(let missing):
            if initialSetupRecoveryUnconfirmed {
                credentialRecoveryMessage = "上次完成结果尚未确认。诊断不授予恢复权限，请重新启动 Switch 后检查。"
                return
            }
            canAttemptInitialSetupCompletion = true
            credentialRecoveryMessage = missing
                ? "发现未完成的首次设置。完成时会重新核对空队列并补全原密钥见证。"
                : "发现未完成的首次设置。点击完成后会重新核对原配置与空队列。"
        case .completed(let payload):
            applyStoredConfiguration(payload.stored.configuration)
            configurationIntent = payload.stored.intent
            appliedConfiguration = payload.stored.configuration
            loadedActiveAccount = payload.account
            credentialScopeLoaded = true
            credentialLoadFailure = nil
            errorMessage = nil
            credentialRecoveryMessage = "首次设置已完成。已保留原设置；请保存并应用后使用转发。"
        case .failed(let failure):
            switch failure {
            case .busy:
                canAttemptInitialSetupCompletion = true
                credentialRecoveryMessage = "其他 Switch 实例正在使用转发凭据或队列，请稍后重试。不会结束进程或删除锁。"
            case .alreadyActive:
                credentialRecoveryMessage = "已有已提交配置，请重新启动 Switch 后读取。当前不会覆盖配置。"
            case .legacyStateUnsupported:
                if mode == .complete { initialSetupRecoveryUnconfirmed = true }
                credentialRecoveryMessage = "发现旧版凭据或中断的旧版迁移，此处无法完成。请使用原版本或人工恢复；已保留全部原数据与凭据。"
            case .invalidEvidence:
                if mode == .complete { initialSetupRecoveryUnconfirmed = true }
                credentialRecoveryMessage = "无法唯一核对未完成设置与原空队列。已保留全部凭据和文件，请重新启动后检查；不会换钥或清空队列。"
            case .unknown:
                initialSetupRecoveryUnconfirmed = true
                let message = "完成结果尚未确认。已保留全部凭据和文件，请重新启动 Switch 后检查；不会重试覆盖。"
                credentialLoadFailure = message
                errorMessage = message
                credentialRecoveryMessage = message
            }
        }
    }

    /// Explicit read-only local diagnosis. Never called by startup, never
    /// selects a stage, never changes appliedConfiguration or error fences.
    /// Existing lock files are required; diagnosis does not create artifacts.
    func diagnoseCredentialRecovery() -> SMSForwardingRecoveryDiagnosis {
        typealias Diagnosis = SMSForwardingRecoveryDiagnosis
        func blocked(_ state: Diagnosis.State) -> Diagnosis {
            Diagnosis(state: state, witness: .unverified, snapshotCount: 0, activePointerPresent: false)
        }
        guard let scope = credentialScope else { return blocked(.unavailable) }
        do {
            return try scope.withLock(createIfMissing: false) {
              try scope.withQueueBootstrapLock(createIfMissing: false) { directory in
                try scope.withLegacyWriterLock(directory: directory, createIfMissing: false) {
                    let active = try Self.readSecret(Key.activeConfigurationAccount, service: scope.service)
                    let accounts = try snapshotAccountsLocked(scope: scope, strict: true).sorted()
                    guard accounts.count <= Self.snapshotBudget,
                          active == nil || accounts.contains(active!) else { return blocked(.invalidSnapshot) }
                    var encodedSnapshots: [String] = []
                    var keyBytes = Data()
                    defer { keyBytes.resetBytes(in: 0..<keyBytes.count) }
                    for account in accounts {
                        guard let encoded = try Self.readSecret(account, service: scope.service),
                              let data = encoded.data(using: .utf8),
                              let stored = try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: data),
                              stored.schemaVersion == 1, stored.intent.clientSequence > 0,
                              !stored.intent.clientRequestID.isEmpty,
                              let key = Data(base64Encoded: stored.configuration.stateKey), key.count == 32 else {
                            return blocked(.invalidSnapshot)
                        }
                        encodedSnapshots.append(encoded); keyBytes.append(key)
                    }
                    // This old key is diagnostic evidence only, never migrated.
                    let legacyKey = try scope.migratesLegacy
                        ? Self.readSecret("state.encryptionKey", service: SMSForwardingCredentialScope.legacyService) : nil
                    if let legacyKey {
                        guard let key = Data(base64Encoded: legacyKey), key.count == 32 else {
                            return blocked(.invalidSnapshot)
                        }
                        keyBytes.append(key)
                    }
                    var observation: UnsafeMutableRawPointer?
                    defer { rustDestroyForwardingDiagnosis(observation) }
                    var results = [UInt8](repeating: 0, count: keyBytes.count / 32)
                    let status = keyBytes.withUnsafeBytes { bytes in
                        results.withUnsafeMutableBufferPointer { output in
                            rustDiagnoseForwardingQueue(directory,
                                bytes.baseAddress?.assumingMemoryBound(to: UInt8.self),
                                output.count, output.baseAddress, &observation)
                        }
                    }
                    guard status >= 0 else { throw SMSForwardingCredentialScope.Failure.unsafePath }
                    #if SWITCH_FORWARDING_TESTS
                    ForwardingKeychain.barrier("diagnostic-captured")
                    #endif
                    // The Rust result is unpublished until both the exact Keychain
                    // set/bytes and outer pinned namespaces have been rechecked.
                    guard try Self.readSecret(Key.activeConfigurationAccount, service: scope.service) == active,
                          try snapshotAccountsLocked(scope: scope, strict: true).sorted() == accounts else {
                        throw SMSForwardingCredentialScope.Failure.stale
                    }
                    for (index, account) in accounts.enumerated() {
                        guard try Self.readSecret(account, service: scope.service) == encodedSnapshots[index] else {
                            throw SMSForwardingCredentialScope.Failure.stale
                        }
                    }
                    if scope.migratesLegacy {
                        guard try Self.readSecret("state.encryptionKey", service: SMSForwardingCredentialScope.legacyService) == legacyKey else {
                            throw SMSForwardingCredentialScope.Failure.stale
                        }
                    }
                    #if SWITCH_FORWARDING_TESTS
                    ForwardingKeychain.barrier("diagnostic-checked")
                    #endif
                    guard rustConfirmForwardingDiagnosis(observation) == 0 else {
                        throw SMSForwardingCredentialScope.Failure.unsafePath
                    }
                    let kind = status & 15
                    let matches = accounts.indices.filter { results[$0] & 15 == 1 || results[$0] & 15 == 2 }
                    let legacyMatches = legacyKey != nil && (results.last! & 15 == 1 || results.last! & 15 == 2)
                    let witness: Diagnosis.Witness
                    if matches.count == 1 {
                        switch results[matches[0]] >> 4 {
                        case 0: witness = .missing
                        case 1: witness = .matching
                        case 2: witness = .mismatched
                        default: witness = .malformed
                        }
                    } else if legacyMatches && matches.isEmpty {
                        switch results.last! >> 4 {
                        case 0: witness = .missing
                        case 1: witness = .matching
                        case 2: witness = .mismatched
                        default: witness = .malformed
                        }
                    } else {
                        witness = status >> 4 == 0 ? .missing : (status >> 4 == 3 ? .malformed : .unverified)
                    }
                    let state: Diagnosis.State
                    if kind == 0 { state = .noSeed }
                    else if kind == 1 { state = .tornSeed }
                    else if kind == 2 { state = .unsupportedQueue }
                    else if results.contains(where: { $0 & 15 == 3 }) { state = .unusableQueue }
                    else if matches.count > 1 { state = .ambiguousCandidates }
                    else if let match = matches.first {
                        if accounts[match] == active { state = .activeQueue }
                        else { state = results[match] & 15 == 1 ? .uniqueStagedSeed : .uniqueStagedQueue }
                    } else { state = legacyMatches ? .legacyKeyQueue : .unmatchedQueue }
                    return Diagnosis(state: state, witness: witness, snapshotCount: accounts.count,
                                     activePointerPresent: active != nil)
                }
              }
            }
        } catch SMSForwardingCredentialScope.Failure.busy { return blocked(.busy) }
        catch SMSForwardingCredentialScope.Failure.unsafePath { return blocked(.unsafeNamespace) }
        catch {
            let failure = error as NSError
            return blocked(failure.domain == "SwitchForwardingCredentials" && failure.code == 11
                           ? .invalidSnapshot : .unavailable)
        }
    }

    private func verifyCurrentCredentialSelection() throws {
        guard let scope = credentialScope, credentialScopeLoaded else {
            throw SMSForwardingCredentialScope.Failure.unsafePath
        }
        try scope.withLock {
            guard try Self.readSecret(Key.activeConfigurationAccount, service: scope.service) == loadedActiveAccount else {
                throw SMSForwardingCredentialScope.Failure.stale
            }
        }
    }

    private func persist() throws -> SMSForwardingConfigurationIntent {
        if let credentialLoadFailure {
            throw NSError(domain: "SwitchForwardingCredentials", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: credentialLoadFailure])
        }
        guard let scope = credentialScope, credentialScopeLoaded else {
            throw SMSForwardingCredentialScope.Failure.unsafePath
        }
        do {
            return try scope.withLock {
                guard try Self.readSecret(Key.activeConfigurationAccount, service: scope.service) == loadedActiveAccount else {
                    throw SMSForwardingCredentialScope.Failure.stale
                }
                let intent = try advancedConfigurationIntent()
                try commitConfigurationLocked(makeConfiguration(), intent: intent, scope: scope)
                return intent
            }
        } catch let failure as SMSForwardingCredentialScope.Failure {
            throw NSError(domain: "SwitchForwardingCredentials", code: 7,
                          userInfo: [NSLocalizedDescriptionKey: credentialCoordinationMessage(failure)])
        }
    }

    private struct StagedConfiguration {
        let account: String
        let encoded: String
        let reclaimedForCapacity: Bool
    }

    /// Stage an immutable scoped snapshot under the credential lock. A stage
    /// alone is never active authority and must survive later seed failures.
    private func stageConfigurationLocked(
        _ configuration: SMSForwardingConfiguration, intent: SMSForwardingConfigurationIntent,
        scope: SMSForwardingCredentialScope
    ) throws -> StagedConfiguration {
        let reclaimedForCapacity = try ensureSnapshotCapacityLocked(scope: scope)
        let stored = SMSForwardingStoredConfiguration(schemaVersion: 1, configuration: configuration, intent: intent)
        let account = Key.snapshotAccountPrefix + UUID().uuidString.lowercased()
        let data = try JSONEncoder().encode(stored)
        guard let encoded = String(data: data, encoding: .utf8) else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 3)
        }
        do {
            guard try Self.readSecret(account, service: scope.service) == nil else {
                throw NSError(domain: "SwitchForwardingCredentials", code: 3)
            }
            try Self.addAndVerifySecret(encoded, name: account, service: scope.service)
            return StagedConfiguration(account: account, encoded: encoded,
                                       reclaimedForCapacity: reclaimedForCapacity)
        } catch {
            let message = "钥匙串配置保存或校验失败。请重新启动 Switch 后检查配置；恢复前不会提交转发配置。"
            credentialLoadFailure = message
            throw NSError(domain: "SwitchForwardingCredentials", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: message])
        }
    }

    /// The active pointer is the only commit point. Verify the exact staged
    /// bytes again after queue/witness sync and before publishing authority.
    private func activateStagedConfigurationLocked(
        _ stage: StagedConfiguration, scope: SMSForwardingCredentialScope,
        expectedActiveAccount: String?
    ) throws {
        do {
            guard try Self.readSecret(stage.account, service: scope.service) == stage.encoded,
                  try Self.readSecret(Key.activeConfigurationAccount, service: scope.service) == expectedActiveAccount else {
                throw SMSForwardingCredentialScope.Failure.stale
            }
            let account = stage.account
            try Self.writeAndVerifySecret(account, name: Key.activeConfigurationAccount, service: scope.service)
            loadedActiveAccount = account
            if !stage.reclaimedForCapacity {
                reclaimSnapshotsLocked(activeAccount: account, encoded: stage.encoded, scope: scope)
            }
        } catch {
            let message = "钥匙串配置保存或校验失败。请重新启动 Switch 后检查配置；恢复前不会提交转发配置。"
            credentialLoadFailure = message
            throw NSError(domain: "SwitchForwardingCredentials", code: 4,
                          userInfo: [NSLocalizedDescriptionKey: message])
        }
    }

    /// Caller holds the fixed real-user coordination lock throughout both
    /// Keychain operations. Existing save/migration semantics remain intact.
    private func commitConfigurationLocked(
        _ configuration: SMSForwardingConfiguration, intent: SMSForwardingConfigurationIntent,
        scope: SMSForwardingCredentialScope
    ) throws {
        let expected = try Self.readSecret(Key.activeConfigurationAccount, service: scope.service)
        let stage = try stageConfigurationLocked(configuration, intent: intent, scope: scope)
        try activateStagedConfigurationLocked(stage, scope: scope, expectedActiveAccount: expected)
    }

    private enum ConfigurationSyncResult {
        case confirmed(String?)
        case stale
        case failed(String)
    }

    /// Sends one explicit durable identity and snapshot. Rust persists and
    /// compares the monotonic sequence before replacing any provider worker;
    /// Swift only projects the response belonging to its current form.
    private func synchronizeConfiguration(
        _ configuration: SMSForwardingConfiguration,
        intent: SMSForwardingConfigurationIntent,
        presentationGeneration: UInt64
    ) async -> ConfigurationSyncResult {
        if let credentialLoadFailure { return .failed(credentialLoadFailure) }
        do {
            try verifyCurrentCredentialSelection()
            let configured: SMSForwardingConfiguredResponse = try await APIClient().send(
                "api/sms/forwarding/configure",
                body: SMSForwardingConfigureRequest(configuration: configuration, intent: intent),
                as: SMSForwardingConfiguredResponse.self)
            try verifyCurrentCredentialSelection()
            guard presentationGeneration == uiSnapshotGeneration,
                  intent == configurationIntent else { return .stale }
            guard configured.clientRequestID == intent.clientRequestID,
                  configured.clientSequence == intent.clientSequence,
                  configured.configurationIntentRevision > 0 else {
                return .failed("本机转发配置意图回执不匹配，请刷新后重试。")
            }
            try verifyRuntimeRoot()
            let observed: SMSForwardingStatus = try await APIClient().get(
                "api/sms/forwarding/status", as: SMSForwardingStatus.self)
            try verifyRuntimeRoot()
            guard presentationGeneration == uiSnapshotGeneration,
                  intent == configurationIntent else { return .stale }
            guard observed.configurationID == configured.configurationID else {
                return .failed("本机转发配置已发生变化，请刷新后重试。")
            }
            confirmedConfigurationID = observed.configurationID
            if isEnabled && confirmedConfigurationID?.isEmpty != false {
                return .failed("本机转发服务未确认配置版本，请重新启动新构建的 Switch。")
            }
            status = observed
            await submitCurrentAdmissionHandoff()
            try verifyRuntimeRoot()
            return .confirmed(observed.configurationID)
        } catch {
            guard presentationGeneration == uiSnapshotGeneration,
                  intent == configurationIntent else { return .stale }
            return .failed(userFacingErrorMessage(error))
        }
    }

    private func presentConfigurationSync(
        _ result: ConfigurationSyncResult,
        presentationGeneration: UInt64
    ) {
        guard presentationGeneration == uiSnapshotGeneration else { return }
        switch result {
        case .confirmed:
            errorMessage = nil
        case .stale:
            break
        case let .failed(message):
            confirmedConfigurationID = nil
            errorMessage = message
        }
    }

    /// Imports the exact last Swift device-SMS and N-wait checkpoints. Rust's
    /// durable device-SMS and N-wait producers start only after this one-time
    /// cursor handoff has been accepted by Rust.
    private func submitCurrentAdmissionHandoff() async {
        guard !admissionHandoffSubmitted, !admissionHandoffInFlight,
              BackendProcess.shared.state == .running else { return }
        do {
            let acknowledgement = try await submitAdmissionHandoff(
                deviceSMS: SMSStore.shared.forwardingAdmissionSnapshot(),
                nwaitSMS: MultiStandbyStore.shared.forwardingAdmissionSnapshot())
            admissionHandoffSubmitted = acknowledgement.accepted
        } catch {
            // Fail closed. Rust producers remain paused until a later explicit
            // status/configuration refresh resubmits the same frozen handoff.
        }
    }

    private func makeConfiguration(stateKey: String? = nil) -> SMSForwardingConfiguration {
        let telegram = SMSForwardingTelegramConfiguration(
            enabled: telegramEnabled, botToken: telegramBotToken, chatID: telegramChatID,
            proxy: SMSForwardingProxyConfiguration(mode: telegramProxyMode, url: telegramProxyURL))
        let bark = SMSForwardingBarkConfiguration(
            enabled: barkEnabled, serverURL: barkServerURL, deviceKey: barkDeviceKey,
            iconURL: barkIconURL, group: barkGroup, encrypted: barkEncrypted,
            encryptionAlgorithm: barkEncryptionAlgorithm, encryptionKey: barkEncryptionKey)
        return SMSForwardingConfiguration(telegram: telegram, bark: bark,
                                          stateKey: stateKey ?? stateEncryptionKey)
    }

    private func applyStoredConfiguration(_ configuration: SMSForwardingConfiguration) {
        telegramEnabled = configuration.telegram.enabled
        telegramBotToken = configuration.telegram.botToken
        telegramChatID = configuration.telegram.chatID
        telegramProxyMode = configuration.telegram.proxy.mode
        telegramProxyURL = configuration.telegram.proxy.url
        barkEnabled = configuration.bark.enabled
        barkServerURL = configuration.bark.serverURL
        barkDeviceKey = configuration.bark.deviceKey
        barkIconURL = configuration.bark.iconURL
        barkGroup = configuration.bark.group
        barkEncrypted = configuration.bark.encrypted
        barkEncryptionAlgorithm = configuration.bark.encryptionAlgorithm
        barkEncryptionKey = configuration.bark.encryptionKey
        stateEncryptionKey = configuration.stateKey
    }

    private func restoredConfigurationIntent() throws -> SMSForwardingConfigurationIntent {
        if let requestID = defaults.string(forKey: Key.configurationIntentID),
           !requestID.isEmpty,
           let sequenceText = defaults.string(forKey: Key.configurationIntentSequence),
           let sequence = UInt64(sequenceText), sequence > 0 {
            return SMSForwardingConfigurationIntent(
                clientRequestID: requestID, clientSequence: sequence)
        }
        return try advancedConfigurationIntent(legacySequence: legacyConfigurationSequence())
    }

    private func legacyConfigurationSequence() -> UInt64 {
        defaults.string(forKey: Key.configurationIntentSequence).flatMap(UInt64.init) ?? 0
    }

    private func advancedConfigurationIntent(legacySequence: UInt64 = 0) throws -> SMSForwardingConfigurationIntent {
        // Global defaults are consulted only by the one-way default-root legacy
        // bootstrap. Every v2 save belongs solely to its loaded scoped snapshot.
        let current = max(legacySequence, configurationIntent.clientSequence)
        let (next, exhausted) = current.addingReportingOverflow(1)
        guard !exhausted else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 8,
                          userInfo: [NSLocalizedDescriptionKey: "转发配置序号已用尽；原配置保持不变，请恢复配置后重试。"])
        }
        return SMSForwardingConfigurationIntent(clientRequestID: UUID().uuidString.lowercased(), clientSequence: next)
    }

    /// Only the Keychain snapshot owns the intent after migration. Legacy
    /// defaults are inputs, never a separate commit that could outlive its key.
    private static func confirmQueueAbsentBeforeGeneratingKey(directory: Int32) throws {
        var metadata = stat()
        // The caller holds the historical writer flock. A stale lock file is
        // safe to reuse; queue ciphertext or witness must never be replaced.
        for name in ["state.enc", "state.enc.keyid"] {
            guard fstatat(directory, name, &metadata, AT_SYMLINK_NOFOLLOW) != 0 else {
                throw NSError(domain: "SwitchForwardingCredentials", code: 6)
            }
            guard errno == ENOENT else {
                throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
            }
        }
    }

    private static let snapshotBudget = 16

    private func snapshotAccountsLocked(scope: SMSForwardingCredentialScope, strict: Bool = false) throws -> [String] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: scope.service,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return [] }
        guard status == errSecSuccess, let attributes = result as? [[String: Any]],
              attributes.allSatisfy({ $0[kSecAttrAccount as String] is String }) else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 9,
                          userInfo: [NSLocalizedDescriptionKey: "无法确认转发凭据存储容量；原配置保持不变，请稍后重试。"])
        }
        let allAccounts = attributes.compactMap { $0[kSecAttrAccount as String] as? String }
        // A diagnostic cannot select an arbitrary item when enumeration reports
        // duplicate identities. Ordinary historical GC keeps its existing policy.
        if strict && Set(allAccounts).count != allAccounts.count {
            throw NSError(domain: "SwitchForwardingCredentials", code: 11)
        }
        if strict && allAccounts.contains(where: {
            $0.hasPrefix(Key.snapshotAccountPrefix)
                && UUID(uuidString: String($0.dropFirst(Key.snapshotAccountPrefix.count))) == nil
        }) { throw NSError(domain: "SwitchForwardingCredentials", code: 11) }
        return Set(allAccounts)
            .filter { $0.hasPrefix(Key.snapshotAccountPrefix)
                && UUID(uuidString: String($0.dropFirst(Key.snapshotAccountPrefix.count))) != nil }
    }

    /// A persistent cleanup failure cannot create unbounded Keychain items.
    /// Capacity must be known before staging; old active is never traded away.
    private func ensureSnapshotCapacityLocked(scope: SMSForwardingCredentialScope) throws -> Bool {
        let accounts = try snapshotAccountsLocked(scope: scope)
        if accounts.count < Self.snapshotBudget { return false }
        if let active = try Self.readSecret(Key.activeConfigurationAccount, service: scope.service),
           let encoded = try Self.readSecret(active, service: scope.service) {
            reclaimSnapshotsLocked(activeAccount: active, encoded: encoded, scope: scope)
        }
        guard try snapshotAccountsLocked(scope: scope).count < Self.snapshotBudget else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 9,
                          userInfo: [NSLocalizedDescriptionKey: "转发凭据存储已达到安全容量；原配置保持不变，请恢复钥匙串访问后重试。"])
        }
        return true
    }

    /// Requires the same fixed-user global lock as every v2 writer. Preserve
    /// active plus two complete lower-sequence recovery generations. Uncommitted
    /// same/newer-sequence stages go first, then oldest generations, at most 8.
    /// Legacy and other DataRoot services are never queried or deleted.
    private func reclaimSnapshotsLocked(activeAccount: String, encoded: String, scope: SMSForwardingCredentialScope) {
        do {
            try scope.verifyRoot()
            guard try Self.readSecret(Key.activeConfigurationAccount, service: scope.service) == activeAccount,
                  try Self.readSecret(activeAccount, service: scope.service) == encoded,
                  let activeData = encoded.data(using: .utf8),
                  let active = try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: activeData) else { return }
            let accounts = try snapshotAccountsLocked(scope: scope).filter { $0 != activeAccount }
            var snapshots: [(account: String, sequence: UInt64)] = []
            for account in accounts {
                guard let value = try Self.readSecret(account, service: scope.service),
                      let data = value.data(using: .utf8),
                      let stored = try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: data),
                      stored.schemaVersion == 1, stored.intent.clientSequence > 0,
                      !stored.intent.clientRequestID.isEmpty,
                      Data(base64Encoded: stored.configuration.stateKey)?.count == 32 else { return }
                snapshots.append((account, stored.intent.clientSequence))
            }
            let sequence = active.intent.clientSequence
            let recovery = Set(snapshots.filter { $0.sequence < sequence }
                .sorted { $0.sequence == $1.sequence ? $0.account < $1.account : $0.sequence > $1.sequence }
                .prefix(2).map { $0.account })
            let discard = snapshots.filter { !recovery.contains($0.account) }.sorted {
                let leftStage = $0.sequence >= sequence, rightStage = $1.sequence >= sequence
                if leftStage != rightStage { return leftStage }
                return $0.sequence == $1.sequence ? $0.account < $1.account : $0.sequence < $1.sequence
            }.prefix(8)
            for candidate in discard {
                try scope.verifyRoot()
                guard try Self.readSecret(Key.activeConfigurationAccount, service: scope.service) == activeAccount,
                      try Self.readSecret(activeAccount, service: scope.service) == encoded else { return }
                let identity: [String: Any] = [
                    kSecClass as String: kSecClassGenericPassword,
                    kSecAttrService as String: scope.service,
                    kSecAttrAccount as String: candidate.account
                ]
                let deletion = SecItemDelete(identity as CFDictionary)
                guard deletion == errSecSuccess || deletion == errSecItemNotFound else { return }
            }
        } catch { return }
    }

    nonisolated fileprivate static func readSecret(_ account: String, service: String) throws -> String? {
        guard let data = try readSecretBytes(account, service: service) else { return nil }
        guard let value = String(data: data, encoding: .utf8) else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(errSecDecode))
        }
        return value
    }

    // Recovery compares original OS bytes. String equality accepts canonically
    // equivalent Unicode and therefore cannot prove an immutable snapshot.
    nonisolated fileprivate static func readSecretBytes(_ account: String, service: String) throws -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
        guard let data = result as? Data else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(errSecDecode))
        }
        return data
    }

    private static func generateStateEncryptionKey() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status),
                          userInfo: [NSLocalizedDescriptionKey: "无法安全生成转发队列密钥；未提交或覆盖配置。"])
        }
        return Data(bytes).base64EncodedString()
    }

    private static func writeSecret(_ value: String, name: String, service: String) -> OSStatus {
        let identity: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: name
        ]
        guard !value.isEmpty else {
            let status = SecItemDelete(identity as CFDictionary)
            return status == errSecItemNotFound ? errSecSuccess : status
        }
        let attributes: [String: Any] = [
            kSecValueData as String: Data(value.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        let status = SecItemUpdate(identity as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            var insertion = identity
            attributes.forEach { insertion[$0.key] = $0.value }
            return SecItemAdd(insertion as CFDictionary, nil)
        }
        return status
    }

    private static func writeAndVerifySecret(_ value: String, name: String, service: String) throws {
        let status = writeSecret(value, name: name, service: service)
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
        guard try readSecret(name, service: service) == value else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 5)
        }
    }

    /// Immutable snapshot creation uses Keychain's add-only primitive. A UUID
    /// collision or concurrent insertion cannot turn staging into an update.
    nonisolated fileprivate static func addAndVerifySecret(_ value: String, name: String, service: String) throws {
        let insertion: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: name,
            kSecValueData as String: Data(value.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        let status = SecItemAdd(insertion as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
        guard try readSecret(name, service: service) == value else {
            throw NSError(domain: "SwitchForwardingCredentials", code: 5)
        }
    }
}
