import Foundation

// MARK: - eSIM

struct ESIMProfileContext: Identifiable {
    let eid: String
    let aidHex: String
    let profile: ProfileItem

    var id: String { "\(aidHex):\(profile.iccid)" }
}

struct ESIMBanner: Identifiable {
    enum Tone {
        case info
        case success
        case warning
        case error
    }

    let id = UUID()
    let tone: Tone
    let message: String
    let detail: String?
}

struct EUICCInfo: Decodable {
    let aid: String?
    let eid: String?
    let spec: String?
    let freeNvram: String?
    let freeNvramBytes: Int32?
    let firmware: String?
    let manufacturer: String?
    let sasAccreditationNumber: String?
    let specGuess: String?
    let specConfidence: String?
    let infoSource: String?
    let infoVersion: String?
    let infoError: String?
    let defaultSMDPAddress: String?
    let rootDSAddress: String?

    enum CodingKeys: String, CodingKey {
        case aid, eid, spec, firmware, manufacturer
        case freeNvram = "free_nvram"
        case freeNvramBytes = "free_nvram_bytes"
        case sasAccreditationNumber = "sas_accreditation_number"
        case specGuess = "spec_guess"
        case specConfidence = "spec_confidence"
        case infoSource = "info_source"
        case infoVersion = "info_version"
        case infoError = "info_error"
        case defaultSMDPAddress = "default_smdp_address"
        case rootDSAddress = "root_ds_address"
    }
}

struct CardIdentity: Decodable {
    let brand: String?
    let model: String?
    let hardwareRevision: String?
    let source: String?
    let confidence: String?
    let ruleID: String?
    let sourceURL: String?
    let sourceVersion: String?
    let evidence: [CardIdentityEvidence]?

    enum CodingKeys: String, CodingKey {
        case brand, model, source, confidence, evidence
        case hardwareRevision = "hardware_revision"
        case ruleID = "rule_id"
        case sourceURL = "source_url"
        case sourceVersion = "source_version"
    }
}

struct CardIdentityEvidence: Decodable {
    let kind: String
    let name: String
    let url: String
    let version: String
}

struct ChipInfo: Decodable {
    let eids: [EUICCInfo]?
    let skuName: String?
    let serialNumber: String?
    let firmware: String?
    let identity: CardIdentity?

    enum CodingKeys: String, CodingKey {
        case eids
        case skuName = "sku_name"
        case serialNumber = "serial_number"
        case firmware, identity
    }
}

struct ProfileItem: Decodable, Identifiable {
    let iccid: String
    let name: String?
    let serviceProviderName: String?
    let state: Int?
    let stateText: String?
    let classText: String?

    var id: String { iccid }
    var isEnabled: Bool { state == 1 }
    var displayName: String {
        if let name = name?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
            return name
        }
        if let provider = serviceProviderName?.trimmingCharacters(in: .whitespacesAndNewlines), !provider.isEmpty {
            return provider
        }
        return iccid.isEmpty ? "Profile" : "Profile · …\(iccid.suffix(6))"
    }

    /// 基于 ICCID 推断国旗 emoji
    var flagEmoji: String? { iccidFlagEmoji(iccid) }

    enum CodingKeys: String, CodingKey {
        case iccid, name, state
        case serviceProviderName = "service_provider_name"
        case stateText = "state_text"
        case classText = "class_text"
    }
}

struct EUICCProfiles: Decodable {
    let eid: String?
    let aidHex: String?
    let readState: String?
    let readError: String?
    let profiles: [ProfileItem]?

    enum CodingKeys: String, CodingKey {
        case eid
        case aidHex = "aid_hex"
        case readState = "read_state"
        case readError = "read_error"
        case profiles
    }
}

struct ESIMCapabilities: Decodable {
    let canRefresh: Bool
    let canDownload: Bool
    let canSwitch: Bool
    let canRename: Bool
    let canDelete: Bool
    let canProbePhonebook: Bool
    let vendorSerialAvailable: Bool

    enum CodingKeys: String, CodingKey {
        case canRefresh = "can_refresh"
        case canDownload = "can_download"
        case canSwitch = "can_switch"
        case canRename = "can_rename"
        case canDelete = "can_delete"
        case canProbePhonebook = "can_probe_phonebook"
        case vendorSerialAvailable = "vendor_serial_available"
    }
}

/// GET /api/esim
struct ESIMOverview: Decodable {
    let cardType: String?
    let message: String?
    let chipInfo: ChipInfo?
    let profiles: [EUICCProfiles]?
    let capabilities: ESIMCapabilities?
    let updatedAt: Date?
    let operation: ESIMOperationSnapshot?
    let refresh: ESIMRefreshOutcome?

    enum CodingKeys: String, CodingKey {
        case message, capabilities, operation, refresh
        case cardType = "card_type"
        case chipInfo = "chip_info"
        case profiles
        case updatedAt = "updated_at"
    }
}

/// GET /api/esim/notes
struct ProfileNote: Decodable {
    let label: String?
    let phone: String?
    let tags: String?
}

func usableSIMPhoneNumber(_ candidate: String?) -> String? {
    guard let value = candidate?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty else { return nil }
    let normalized = value.lowercased()
    if ["-", "unknown", "unknown number", "n/a", "号码未知", "未知号码"].contains(normalized) { return nil }
    let digits = value.filter(\.isNumber)
    if digits.isEmpty || digits.allSatisfy({ $0 == "0" }) { return nil }
    return value
}

struct NotesResponse: Decodable {
    let notes: [String: ProfileNote]?
}

/// PUT /api/esim/notes
struct SaveNoteRequest: Encodable {
    let iccid: String
    let label: String
    let phone: String
    let tags: String
}

struct MessageResponse: Decodable {
    let message: String?
    let warning: String?
    let warningCode: String?

    enum CodingKeys: String, CodingKey {
        case message
        case warning
        case warningCode = "warning_code"
    }
}

/// GET /api/esim/module-notes
struct ModuleProfileNote: Decodable {
    let index: Int?
    let iccid: String?
    let label: String?
    let phone: String?
    let tags: String?
}

struct ModuleNotesResponse: Decodable {
    let notes: [String: ModuleProfileNote]?
    let used: Int?
    let total: Int?
}

/// PUT /api/esim/module-notes
struct SaveModuleNoteRequest: Encodable {
    let iccid: String
    let label: String
    let phone: String
    let tags: String
}

/// POST /api/esim/phonebook/probe
struct PhonebookProbeResult: Decodable {
    let storage: String?
    let storageSupported: Bool?
    let storageSelected: Bool?
    let readSupported: Bool?
    let writeSupported: Bool?
    let storageStatus: String?
    let responses: [String: String]?

    enum CodingKeys: String, CodingKey {
        case storage, responses
        case storageSupported = "storage_supported"
        case storageSelected = "storage_selected"
        case readSupported = "read_supported"
        case writeSupported = "write_supported"
        case storageStatus = "storage_status"
    }
}

/// POST /api/esim/switch
struct ESIMSwitchRequest: Encodable {
    let iccid: String
    let aid: String?
    let clientRequestID: String

    enum CodingKeys: String, CodingKey {
        case iccid, aid
        case clientRequestID = "client_request_id"
    }
}

struct ESIMSwitchResult: Decodable {
    let switchAccepted: Bool?
    let phase: String?
    let targetIccid: String?
    let recoveryPending: Bool?
    let moduleRebootRequested: Bool?
    let moduleRebootResponse: String?
    let moduleRebootWarning: String?
    let reconnectWaitSeconds: Int?
	let operation: ESIMOperationSnapshot?
	let readerInfo: ReaderInfoResponse?

    enum CodingKeys: String, CodingKey {
		case phase, operation
		case readerInfo = "reader_info"
        case switchAccepted = "switch_accepted"
        case targetIccid = "target_iccid"
        case recoveryPending = "recovery_pending"
        case moduleRebootRequested = "module_reboot_requested"
        case moduleRebootResponse = "module_reboot_response"
        case moduleRebootWarning = "module_reboot_warning"
        case reconnectWaitSeconds = "reconnect_wait_seconds"
    }
}

/// PATCH /api/esim/profile
struct ESIMRenameRequest: Encodable {
    let iccid: String
    let aid: String?
    let name: String
    let clientRequestID: String

    enum CodingKeys: String, CodingKey {
        case iccid, aid, name
        case clientRequestID = "client_request_id"
    }
}

/// DELETE /api/esim/profile
struct ESIMDeleteRequest: Encodable {
    let iccid: String
    let aid: String?
    let clientRequestID: String

    enum CodingKeys: String, CodingKey {
        case iccid, aid
        case clientRequestID = "client_request_id"
    }
}

struct ESIMMutationIntentReceipt: Decodable {
    let clientRequestID: String
    let action: String
    let scope: String
    let phase: String
    let operationID: String
    let dispatchCount: Int
    let responseStatus: Int?
    let responseOperation: ESIMOperationSnapshot?

    enum CodingKeys: String, CodingKey {
        case action, scope, phase
        case clientRequestID = "client_request_id"
        case operationID = "operation_id"
        case dispatchCount = "dispatch_count"
        case responseStatus = "response_status"
        case response
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        clientRequestID = try values.decode(String.self, forKey: .clientRequestID)
        action = try values.decode(String.self, forKey: .action)
        scope = try values.decode(String.self, forKey: .scope)
        phase = try values.decode(String.self, forKey: .phase)
        operationID = try values.decode(String.self, forKey: .operationID)
        dispatchCount = try values.decode(Int.self, forKey: .dispatchCount)
        responseStatus = try values.decodeIfPresent(Int.self, forKey: .responseStatus)
        // Other mutations store different response bodies. Only a download's
        // HTTP 202 body can be used to resume operation tracking.
        responseOperation = action == "download" && responseStatus == 202
            ? try? values.decode(ESIMOperationSnapshot.self, forKey: .response)
            : nil
    }

    var isTerminal: Bool { phase == "terminal" }
    var blocksNewMutation: Bool { !isTerminal }
    var succeeded: Bool { responseStatus.map { (200..<300).contains($0) && $0 != 202 } == true }
    var acceptedDownloadOperation: ESIMOperationSnapshot? {
        guard isTerminal, action == "download", responseStatus == 202,
              let responseOperation, !responseOperation.id.isEmpty,
              responseOperation.kind == "download_profile", responseOperation.isActive else { return nil }
        return responseOperation
    }
}

struct ESIMMutationCheckpoint: Codable, Equatable {
    let scope: String
    let action: String
    let clientRequestID: String
}

enum ESIMMutationCheckpointLoad: Equatable {
    case missing
    case valid(ESIMMutationCheckpoint)
    case corrupt(String)
}

protocol ESIMCheckpointDefaults: AnyObject {
    func object(forKey defaultName: String) -> Any?
    func set(_ value: Any?, forKey defaultName: String)
    func removeObject(forKey defaultName: String)
    func synchronize() -> Bool
}

extension UserDefaults: ESIMCheckpointDefaults {}

/// The GUI keeps the exact request ID across a process restart.  A transport
/// error never removes this fence; only an exact terminal Rust receipt does.
final class ESIMMutationCheckpointStore {
    static let shared = ESIMMutationCheckpointStore()

    private let defaults: ESIMCheckpointDefaults
    private let keyPrefix: String
    private var persistenceFailures: [String: String] = [:]

    init(defaults: ESIMCheckpointDefaults = UserDefaults.standard, keyPrefix: String? = nil) {
        self.defaults = defaults
        let owner = Bundle.main.bundleIdentifier
            ?? "cli.\(ProcessInfo.processInfo.processIdentifier).\(UUID().uuidString.lowercased())"
        self.keyPrefix = keyPrefix ?? "switch.esim.mutation.v1.\(owner)"
    }

    func load(scope: String) -> ESIMMutationCheckpointLoad {
        if let persistenceFailure = persistenceFailures[scope] { return .corrupt(persistenceFailure) }
        guard let stored = defaults.object(forKey: key(scope: scope)) else { return .missing }
        guard let data = stored as? Data else { return .corrupt("checkpoint is not Data (scope: \(scope))") }
        do {
            let checkpoint = try JSONDecoder().decode(ESIMMutationCheckpoint.self, from: data)
            guard Self.actions(for: scope).contains(checkpoint.action), checkpoint.scope == scope,
                  !checkpoint.clientRequestID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                return .corrupt("checkpoint scope/action/request ID mismatch (scope: \(scope))")
            }
            return .valid(checkpoint)
        } catch {
            return .corrupt("checkpoint JSON invalid (scope: \(scope)): \(error.localizedDescription)")
        }
    }

    func begin(scope: String, action: String) -> ESIMMutationCheckpoint? {
        switch load(scope: scope) {
        case .valid(let existing):
            guard confirmStored(existing) else { return nil }
            return existing
        case .corrupt: return nil // Never overwrite evidence or mint a successor ID.
        case .missing: break
        }
        guard Self.actions(for: scope).contains(action) else { return nil }
        let checkpoint = ESIMMutationCheckpoint(
            scope: scope,
            action: action,
            clientRequestID: UUID().uuidString.lowercased())
        guard let data = try? JSONEncoder().encode(checkpoint) else { return nil }
        defaults.set(data, forKey: key(scope: scope))
        guard confirmStored(checkpoint) else { return nil }
        return checkpoint
    }

    @discardableResult
    func clear(_ checkpoint: ESIMMutationCheckpoint) -> Bool {
        guard load(scope: checkpoint.scope) == .valid(checkpoint),
              let retained = defaults.object(forKey: key(scope: checkpoint.scope)) as? Data else {
            return false
        }
        defaults.removeObject(forKey: key(scope: checkpoint.scope))
        guard defaults.synchronize(), load(scope: checkpoint.scope) == .missing else {
            defaults.set(retained, forKey: key(scope: checkpoint.scope))
            _ = defaults.synchronize()
            persistenceFailures[checkpoint.scope] =
                "checkpoint clear synchronize/readback failed (scope: \(checkpoint.scope))"
            return false
        }
        return true
    }

    private func confirmStored(_ checkpoint: ESIMMutationCheckpoint) -> Bool {
        guard persistenceFailures[checkpoint.scope] == nil,
              let retained = defaults.object(forKey: key(scope: checkpoint.scope)) as? Data,
              defaults.synchronize(),
              defaults.object(forKey: key(scope: checkpoint.scope)) as? Data == retained,
              load(scope: checkpoint.scope) == .valid(checkpoint) else {
            persistenceFailures[checkpoint.scope] =
                "checkpoint save synchronize/readback failed (scope: \(checkpoint.scope))"
            return false
        }
        return true
    }

    private static func actions(for scope: String) -> Set<String> {
        switch scope {
        case "reader": return ["switch", "delete", "rename", "download"]
        case "module": return ["switch", "delete", "download"]
        default: return []
        }
    }

    private func key(scope: String) -> String { "\(keyPrefix).\(scope)" }
}

struct ESIMNotificationMutationCheckpoint: Codable, Equatable {
    let itemID: String
    let sequenceNumber: Int64
    let aid: String?
    let clientRequestID: String
}

enum ESIMNotificationCheckpointLoad: Equatable {
    case missing
    case valid(ESIMNotificationMutationCheckpoint)
    case corrupt(String)
}

/// Durable GUI half of the Rust notification receipt.  A transport loss keeps
/// the exact request ID/sequence/AID for the next user retry.  Corrupt or
/// non-Data evidence and any unconfirmed save/clear are sticky fail-closed
/// fences and are never overwritten with a successor ID.
final class ESIMNotificationMutationCheckpointStore {
    private let defaults: ESIMCheckpointDefaults
    private let key: String
    private var persistenceFailure: String?

    init(defaults: ESIMCheckpointDefaults = UserDefaults.standard, key: String? = nil) {
        self.defaults = defaults
        let owner = Bundle.main.bundleIdentifier
            ?? "cli.\(ProcessInfo.processInfo.processIdentifier).\(UUID().uuidString.lowercased())"
        self.key = key ?? "switch.reader.esim.notification.mutation.v1.\(owner)"
    }

    func load() -> ESIMNotificationCheckpointLoad {
        if let persistenceFailure { return .corrupt(persistenceFailure) }
        guard let stored = defaults.object(forKey: key) else { return .missing }
        guard let data = stored as? Data else {
            return .corrupt("notification checkpoint is not Data")
        }
        guard let checkpoint = try? JSONDecoder().decode(
            ESIMNotificationMutationCheckpoint.self, from: data),
              Self.valid(checkpoint) else {
            return .corrupt("notification checkpoint is invalid")
        }
        return .valid(checkpoint)
    }

    func begin(_ item: ESIMNotificationItem) -> ESIMNotificationMutationCheckpoint? {
        switch load() {
        case .valid(let existing):
            guard existing.itemID == item.id,
                  existing.sequenceNumber == item.sequenceNumber,
                  existing.aid == item.aidHex,
                  confirmStored(existing) else { return nil }
            return existing
        case .corrupt:
            return nil
        case .missing:
            break
        }
        let checkpoint = ESIMNotificationMutationCheckpoint(
            itemID: item.id,
            sequenceNumber: item.sequenceNumber,
            aid: item.aidHex,
            clientRequestID: UUID().uuidString.lowercased())
        guard Self.valid(checkpoint), let data = try? JSONEncoder().encode(checkpoint) else {
            return nil
        }
        defaults.set(data, forKey: key)
        guard confirmStored(checkpoint) else { return nil }
        return checkpoint
    }

    @discardableResult
    func clear(_ checkpoint: ESIMNotificationMutationCheckpoint) -> Bool {
        guard load() == .valid(checkpoint),
              let retained = defaults.object(forKey: key) as? Data else { return false }
        defaults.removeObject(forKey: key)
        guard defaults.synchronize(), load() == .missing else {
            defaults.set(retained, forKey: key)
            _ = defaults.synchronize()
            persistenceFailure = "notification checkpoint clear synchronize/readback failed"
            return false
        }
        return true
    }

    private func confirmStored(_ checkpoint: ESIMNotificationMutationCheckpoint) -> Bool {
        guard persistenceFailure == nil,
              let retained = defaults.object(forKey: key) as? Data,
              defaults.synchronize(),
              defaults.object(forKey: key) as? Data == retained,
              load() == .valid(checkpoint) else {
            persistenceFailure = "notification checkpoint save synchronize/readback failed"
            return false
        }
        return true
    }

    private static func valid(_ checkpoint: ESIMNotificationMutationCheckpoint) -> Bool {
        checkpoint.sequenceNumber > 0
            && checkpoint.itemID == "\(checkpoint.aid ?? ""):\(checkpoint.sequenceNumber)"
            && validUUID(checkpoint.clientRequestID)
            && checkpoint.aid.map {
                !$0.isEmpty && $0.utf8.count <= 512 && $0.utf8.count.isMultiple(of: 2)
                    && $0.utf8.allSatisfy {
                        (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
                    }
            } != false
    }

    private static func validUUID(_ value: String) -> Bool {
        let bytes = Array(value.utf8)
        guard bytes.count == 36 else { return false }
        for (index, byte) in bytes.enumerated() {
            if [8, 13, 18, 23].contains(index) {
                if byte != 45 { return false }
            } else if !((48...57).contains(byte) || (65...70).contains(byte) || (97...102).contains(byte)) {
                return false
            }
        }
        return true
    }
}

/// 删除/下载 Profile 的结果。
struct SpaceDelta: Decodable {
    let direction: String?
    let bytes: Int?
}

struct ESIMProfileResult: Decodable {
    let warning: String?
    let warningCode: String?
    let spaceDelta: SpaceDelta?

    enum CodingKeys: String, CodingKey {
        case warning
        case warningCode = "warning_code"
        case spaceDelta = "space_delta"
    }
}

struct ESIMOperationResult: Decodable {
    let warning: String?
    let warningCode: String?
    let spaceDelta: SpaceDelta?

    enum CodingKeys: String, CodingKey {
        case warning
        case warningCode = "warning_code"
        case spaceDelta = "space_delta"
    }
}

struct ESIMOperationSnapshot: Decodable, Identifiable {
    let id: String
    let kind: String
    let state: String
    let step: String?
    let message: String?
    let progress: Int
    let estimatedProfileSizeBytes: Int64?
    let targetICCID: String?
    let errorCode: String?
    let error: String?
    let diagnostic: String?
    let recoverable: Bool?
    let refreshAfterSeconds: Int?
    let result: ESIMOperationResult?
    let startedAt: Date
    let updatedAt: Date
    let finishedAt: Date?

    var isActive: Bool { state == "queued" || state == "running" || state == "awaiting_confirmation" }
    var awaitsDownloadConfirmation: Bool { state == "awaiting_confirmation" }
    var isCancelled: Bool { state == "cancelled" }
    var isSuccessful: Bool { state == "succeeded" }
    var hasWarning: Bool { state == "warning" }
    var isFailed: Bool { state == "failed" }
    /// Only these states are a Rust-confirmed terminal outcome. Any future or
    /// malformed state must fail closed: the UI may keep observing it, but it
    /// must not infer that the card-write owner released its lease.
    var isKnownTerminal: Bool { isCancelled || isSuccessful || hasWarning || isFailed }
    var hasUnknownOutcome: Bool { !isActive && !isKnownTerminal }
    var blocksNewMutation: Bool { isActive || hasUnknownOutcome }

    enum CodingKeys: String, CodingKey {
        case id, kind, state, step, message, progress, error, diagnostic, recoverable, result
        case estimatedProfileSizeBytes = "estimated_profile_size_bytes"
        case targetICCID = "target_iccid"
        case errorCode = "error_code"
        case refreshAfterSeconds = "refresh_after_seconds"
        case startedAt = "started_at"
        case updatedAt = "updated_at"
        case finishedAt = "finished_at"
    }
}

struct ESIMOperationResponse: Decodable {
    let operation: ESIMOperationSnapshot?
    let manualAckAvailable: Bool?
    /// Preserve the server's exact RFC3339 text for acknowledgement CAS.
    /// Re-encoding Date would lose sub-second precision and reject a valid review.
    let rawUpdatedAt: String?

    private enum CodingKeys: String, CodingKey {
        case operation
        case manualAckAvailable = "manual_ack_available"
    }
    private enum OperationKeys: String, CodingKey { case updatedAt = "updated_at" }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        operation = try container.decodeIfPresent(ESIMOperationSnapshot.self, forKey: .operation)
        manualAckAvailable = try container.decodeIfPresent(Bool.self, forKey: .manualAckAvailable)
        if container.contains(.operation), !(try container.decodeNil(forKey: .operation)) {
            let operationContainer = try container.nestedContainer(keyedBy: OperationKeys.self, forKey: .operation)
            rawUpdatedAt = try operationContainer.decodeIfPresent(String.self, forKey: .updatedAt)
        } else {
            rawUpdatedAt = nil
        }
    }
}

struct ESIMLegacyRecoveryAcknowledgement: Encodable {
    let operationID: String
    let expectedUpdatedAt: String
    let expectedState = "failed"
    let expectedErrorCode = "esim_operation_interrupted"
    let legacyManualAck = true

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case expectedUpdatedAt = "expected_updated_at"
        case expectedState = "expected_state"
        case expectedErrorCode = "expected_error_code"
        case legacyManualAck = "legacy_manual_ack"
    }
}

enum ESIMLegacyRecoveryConfirmation {
    static func displayedIDSuffix(_ operationID: String) -> String {
        String(operationID.suffix(6))
    }

    static func canSubmit(reviewed: Bool, enteredIDSuffix: String, operationID: String) -> Bool {
        guard reviewed, !operationID.isEmpty else { return false }
        return enteredIDSuffix.trimmingCharacters(in: .whitespacesAndNewlines)
            .caseInsensitiveCompare(displayedIDSuffix(operationID)) == .orderedSame
    }
}

/// Pending GSMA SGP.22 notification stored by the eUICC. This is a protocol
/// notification for the operator/SM-DP+, not a macOS user notification.
struct ESIMNotificationItem: Decodable, Identifiable, Equatable {
    let sequenceNumber: Int64
    let event: String
    let iccid: String?
    let address: String?
    let aidHex: String?
    let canRetry: Bool

    var id: String { "\(aidHex ?? ""):\(sequenceNumber)" }

    enum CodingKeys: String, CodingKey {
        case event, iccid, address
        case sequenceNumber = "sequence_number"
        case aidHex = "aid_hex"
        case canRetry = "can_retry"
    }
}

struct ESIMNotificationListResponse: Decodable {
    let notifications: [ESIMNotificationItem]
}

struct ESIMNotificationRetryRequest: Encodable {
    let clientRequestID: String
    let sequenceNumber: Int64
    let aid: String?

    enum CodingKeys: String, CodingKey {
        case clientRequestID = "client_request_id"
        case sequenceNumber = "sequence_number"
        case aid
    }
}

/// POST /api/esim/download
struct ESIMDownloadRequest: Encodable {
    let smdp: String
    let matchingID: String?
    let confirmationCode: String?
    let aid: String?
    let imei: String?
    let clientRequestID: String?

    init(
        smdp: String,
        matchingID: String?,
        confirmationCode: String?,
        aid: String?,
        imei: String?,
        clientRequestID: String? = nil
    ) {
        self.smdp = smdp
        self.matchingID = matchingID
        self.confirmationCode = confirmationCode
        self.aid = aid
        self.imei = imei
        self.clientRequestID = clientRequestID
    }

    func identified(by clientRequestID: String) -> ESIMDownloadRequest {
        ESIMDownloadRequest(
            smdp: smdp,
            matchingID: matchingID,
            confirmationCode: confirmationCode,
            aid: aid,
            imei: imei,
            clientRequestID: clientRequestID)
    }

    enum CodingKeys: String, CodingKey {
        case smdp, aid, imei
        case matchingID = "matching_id"
        case confirmationCode = "confirmation_code"
        case clientRequestID = "client_request_id"
    }
}

struct ESIMDownloadConfirmationRequest: Encodable {
    let operationID: String
    let confirm: Bool

    enum CodingKeys: String, CodingKey {
        case confirm
        case operationID = "operation_id"
    }
}

/// GET /api/vowifi
struct VoWiFiStatus: Decodable, Equatable {
    // 操作状态
    let state: String
    let summary: String
    let detail: String?

    // 飞行模式
    let airplaneMode: Bool
    let cfunMode: Int

    // USB 网络模式
    let usbnetMode: Int
    let usbnetReady: Bool

    // AT 层面
    let imsWiFiCallingEnabled: Bool
    let imsCallModeSet: Bool
    let wifiCallingMode: Int

    // ePDG 配置
    let epdgAddress: String?
    let epdgIPAddress: String?
    let epdgAutoDetect: Bool
    let epdgFqdn: String?

    // 运营商信息
    let operatorMCC: String?
    let operatorMNC: String?
    let operatorName: String?
    /// ICCID owning this module VoWiFi session; reader statuses omit it.
    let simICCID: String?

    // IMS 注册状态
    let imsRegistered: Bool

    // VoWiFi 综合激活状态
    let vowifiActive: Bool

    // Host-routed startup pipeline. Optional fields preserve compatibility
    // with an older local daemon while the app/backend are being upgraded.
    let progressStep: Int?
    let progressDesc: String?
    let smsReady: Bool?
    let callReady: Bool?

    // QMI 辅助（可能不可用）
    let qmiEnabled: Bool?
    let qmiWiFiOnly: Bool?
    let qmiWiFiFirst: Bool?
    let qmiInUse: Bool?
    let qmiWiFiRegistered: Bool?
    let qmiError: String?

    enum CodingKeys: String, CodingKey {
        case state
        case summary
        case detail
        case airplaneMode = "airplane_mode"
        case cfunMode = "cfun_mode"
        case usbnetMode = "usbnet_mode"
        case usbnetReady = "usbnet_ready"
        case imsWiFiCallingEnabled = "ims_wifi_calling_enabled"
        case imsCallModeSet = "ims_call_mode_set"
        case wifiCallingMode = "wifi_calling_mode"
        case epdgAddress = "epdg_address"
        case epdgIPAddress = "epdg_ip_address"
        case epdgAutoDetect = "epdg_auto_detect"
        case epdgFqdn = "epdg_fqdn"
        case operatorMCC = "operator_mcc"
        case operatorMNC = "operator_mnc"
        case operatorName = "operator_name"
        case simICCID = "sim_iccid"
        case imsRegistered = "ims_registered"
        case vowifiActive = "vowifi_active"
        case progressStep = "progress_step"
        case progressDesc = "progress_desc"
        case smsReady = "sms_ready"
        case callReady = "call_ready"
        case qmiEnabled = "qmi_enabled"
        case qmiWiFiOnly = "qmi_wifi_only"
        case qmiWiFiFirst = "qmi_wifi_first"
        case qmiInUse = "qmi_in_use"
        case qmiWiFiRegistered = "qmi_wifi_registered"
        case qmiError = "qmi_error"
    }
}

/// Read-only projection of the retained Rust owner. Observation expiry never
/// certifies a stopped session or released resources.
struct VoWiFiOperation: Decodable, Equatable {
    let operationID: String
    let generation: UInt64
    let clientRequestID: String?
    let requestedEnable: Bool
    let phase: String
    let outcome: String?
    let terminal: Bool
    let succeeded: Bool
    let observationExpired: Bool
    let cleanupConfirmed: Bool
    let pollAfterMS: UInt64
    let message: String

    enum CodingKeys: String, CodingKey {
        case generation, phase, outcome, terminal, succeeded, message
        case operationID = "operation_id"
        case clientRequestID = "client_request_id"
        case requestedEnable = "requested_enable"
        case observationExpired = "observation_expired"
        case cleanupConfirmed = "cleanup_confirmed"
        case pollAfterMS = "poll_after_ms"
    }

    func matches(_ other: VoWiFiOperation) -> Bool {
        operationID == other.operationID && generation == other.generation
    }

    var observationQuery: String {
        // IDs are generated by Rust, but encode them as an opaque query value.
        var query = URLComponents()
        query.queryItems = [
            URLQueryItem(name: "observe_operation", value: "true"),
            URLQueryItem(name: "operation_id", value: operationID),
            URLQueryItem(name: "operation_generation", value: String(generation)),
            clientRequestID.map { URLQueryItem(name: "client_request_id", value: $0) }
        ]
        .compactMap { $0 }
        return query.percentEncodedQuery ?? ""
    }

    static func requestObservationQuery(clientRequestID: String) -> String {
        var query = URLComponents()
        query.queryItems = [
            URLQueryItem(name: "observe_operation", value: "true"),
            URLQueryItem(name: "client_request_id", value: clientRequestID)
        ]
        return query.percentEncodedQuery ?? ""
    }

    /// Preserve the Rust owner identity while a waiter or projection is
    /// temporarily unavailable. This synthetic value is display-only and can
    /// never certify success, cleanup, or release.
    static func pendingObservation(from owner: VoWiFiOperation) -> VoWiFiOperation {
        VoWiFiOperation(
            operationID: owner.operationID,
            generation: owner.generation,
            clientRequestID: owner.clientRequestID,
            requestedEnable: owner.requestedEnable,
            phase: owner.phase,
            outcome: nil,
            terminal: false,
            succeeded: false,
            observationExpired: true,
            cleanupConfirmed: false,
            pollAfterMS: 1_500,
            message: "操作结果待确认；后台 owner 仍在工作")
    }
}

struct VoWiFiAccepted: Decodable {
    let operation: VoWiFiOperation?
}

/// POST /api/vowifi
struct VoWiFiRequest: Encodable {
    let enable: Bool?
    let wifiCallingMode: Int?
    let proxy: String?
    let proxyHost: String?
    let proxyPort: String?
    let epdg: String?
    let epdgPort: Int?
    let epdgPortText: String?
    var observeOperation: Bool? = nil
    var operationID: String? = nil
    var operationGeneration: UInt64? = nil
    var clientRequestID: String? = nil

    enum CodingKeys: String, CodingKey {
        case enable, proxy, epdg
        case wifiCallingMode = "wifi_calling_mode"
        case proxyHost = "proxy_host"
        case proxyPort = "proxy_port"
        case epdgPort = "epdg_port"
        case epdgPortText = "epdg_port_text"
        case observeOperation = "observe_operation"
        case operationID = "operation_id"
        case operationGeneration = "operation_generation"
        case clientRequestID = "client_request_id"
    }

    init(enable: Bool?, wifiCallingMode: Int?, proxy: String? = nil,
         proxyHost: String? = nil, proxyPort: String? = nil,
         epdg: String? = nil, epdgPort: Int? = nil, epdgPortText: String? = nil,
         observeOperation: Bool? = nil, operationID: String? = nil,
         operationGeneration: UInt64? = nil, clientRequestID: String? = nil) {
        self.enable = enable
        self.wifiCallingMode = wifiCallingMode
        self.proxy = proxy
        self.proxyHost = proxyHost
        self.proxyPort = proxyPort
        self.epdg = epdg
        self.epdgPort = epdgPort
        self.epdgPortText = epdgPortText
        self.observeOperation = observeOperation
        self.operationID = operationID
        self.operationGeneration = operationGeneration
        self.clientRequestID = clientRequestID
    }
}

/// GET /api/vowifi/result
struct VoWiFiResult: Decodable, Equatable {
    let done: Bool
    let error: String?
    let status: VoWiFiStatus?
    var operation: VoWiFiOperation? = nil
}

/// GET /api/esim/health
struct ESIMHealth: Decodable {
    let ok: Bool?
    let cardType: String?
    let state: String?
    let message: String?
    let activeProfile: ProfileItem?
    let moduleIccid: String?
    let imsi: String?
    let operatorName: String?
    let registration: String?
    let registered: Bool?
    let signalDbm: Int?
    let networkMode: String?
    let profileMatchesModule: Bool?
    let operation: ESIMOperationSnapshot?

    enum CodingKeys: String, CodingKey {
        case ok, state, message, registration, registered, imsi, operation
        case cardType = "card_type"
        case activeProfile = "active_profile"
        case moduleIccid = "module_iccid"
        case operatorName = "operator"
        case signalDbm = "signal_dbm"
        case networkMode = "network_mode"
        case profileMatchesModule = "profile_matches_module"
    }
}
