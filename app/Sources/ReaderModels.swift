import Foundation

// MARK: - 读卡器（PC/SC CCID）

/// All data derived from the currently observed reader/card generation.
/// Replacing this value publishes one coherent SwiftUI frame, preventing a
/// new profile name from appearing beside the previous profile's number or
/// IMS state during hot-plug and eSIM switching.
struct ReaderRuntimeState {
    var revision: UInt64
    var readers: [String]
    var listLoaded: Bool
    var connected: Bool
    var name: String?
    var usimInfo: USIMInfo?
    var profileOperatorName: String?
    var epdgFQDN: String?
    var carrierConfigError: String?
    var hasESIM: Bool
    var cardType: String?
    var error: String?
    var actionInFlight: Bool
    var voWiFiStatus: ReaderVoWiFiStatus?
    var voWiFiLoaded: Bool
    var voWiFiActionInFlight: Bool

    static let initial = ReaderRuntimeState(
        revision: 0,
        readers: [],
        listLoaded: false,
        connected: false,
        name: nil,
        usimInfo: nil,
        profileOperatorName: nil,
        epdgFQDN: nil,
        carrierConfigError: nil,
        hasESIM: false,
        cardType: nil,
        error: nil,
        actionInFlight: false,
        voWiFiStatus: nil,
        voWiFiLoaded: false,
        voWiFiActionInFlight: false)
}

/// GET /api/v2/device-snapshot — Rust 网关的版本化只读聚合。
/// PC/SC 枚举由 Rust 直接提供；读卡器身份、eSIM 与实时 VoWiFi
/// 会话在完成所有权迁移前仍由兼容硬件进程提供。
struct DeviceSnapshotResponse: Decodable {
    let schemaVersion: Int
    let instanceID: String?
    let revision: UInt64
    let ok: Bool
    let complete: Bool
    let capturedAtMS: UInt64
    let source: String
    let module: ModuleDeviceSnapshot
    let reader: ReaderDeviceSnapshot
    let issues: [DeviceSnapshotIssue]

    enum CodingKeys: String, CodingKey {
        case revision, ok, complete, source, module, reader, issues
        case schemaVersion = "schema_version"
        case instanceID = "instance_id"
        case capturedAtMS = "captured_at_ms"
    }
}

struct ModuleDeviceSnapshot: Decodable {
    let health: HealthStatus?
    let status: DeviceStatus?
}

struct ReaderDeviceSnapshot: Decodable {
    let readers: [String]
    let selected: String?
    let info: ReaderInfoResponse?
    let vowifi: ReaderVoWiFiStatus?
    let inventorySource: String?
    let inventoryRevision: UInt64?
    let readerGeneration: UInt64?
    let devices: [ReaderDiscoveryDevice]?
    var autoConnect: ReaderAutoConnect? = nil

    enum CodingKeys: String, CodingKey {
        case readers, selected, info, vowifi, devices
        case inventorySource = "inventory_source"
        case inventoryRevision = "inventory_revision"
        case readerGeneration = "reader_generation"
        case autoConnect = "auto_connect"
    }
}

struct ReaderDiscoveryDevice: Decodable {
    let name: String
    let cardState: String
    let inUse: Bool
    let muted: Bool
    let atrHex: String

    enum CodingKeys: String, CodingKey {
        case name, muted
        case cardState = "card_state"
        case inUse = "in_use"
        case atrHex = "atr_hex"
    }

}

/// Rust's same-inventory proposal. The lifecycle owner validates every field
/// again before connecting; this DTO grants no local selection authority.
struct ReaderAutoConnect: Codable, Equatable {
    let reader: String
    let inventoryRevision: UInt64
    let readerGeneration: UInt64
    let instanceID: String
    enum CodingKeys: String, CodingKey {
        case reader
        case inventoryRevision = "inventory_revision", readerGeneration = "reader_generation", instanceID = "instance_id"
    }
}

struct DeviceSnapshotIssue: Decodable {
    let endpoint: String
    let code: String
    let message: String
    let retryable: Bool
}

/// GET /api/reader/list
struct ReaderListResponse: Decodable {
    let readers: [String]
    let selected: String?
    let devices: [ReaderDiscoveryDevice]?
    var inventoryRevision: UInt64? = nil
    var readerGeneration: UInt64? = nil
    var instanceID: String? = nil
    var autoConnect: ReaderAutoConnect? = nil
    enum CodingKeys: String, CodingKey {
        case readers, selected, devices
        case inventoryRevision = "inventory_revision", readerGeneration = "reader_generation"
        case instanceID = "instance_id"
        case autoConnect = "auto_connect"
    }
}

/// GET /api/reader/info
struct ReaderInfoResponse: Decodable {
    let status: ReaderStatus
    let usimInfo: USIMInfo?
    let epdgFqdn: String?
    let carrierConfigError: String?

    enum CodingKeys: String, CodingKey {
        case status
        case usimInfo = "usim_info"
        case epdgFqdn = "epdg_fqdn"
        case carrierConfigError = "carrier_config_error"
    }
}

struct ReaderStatus: Decodable {
    let connected: Bool
    let readerName: String?
    let readerGeneration: UInt64?
    let hasCard: Bool
    let cardType: String?
    let hasUSIMInfo: Bool
    let hasESIM: Bool
    // Optional for older backends. A selected transport, physical presence
    // and a verified SIM identity are independent observations.
    let cardState: String?
    let presenceSource: String?
    let presenceError: String?
    let cardMuted: Bool?
    let cardUnpowered: Bool?
    let cardReady: Bool?
    let sessionReady: Bool?
    let metadataReady: Bool?

    var hasConfirmedCardInterruption: Bool {
        cardState == "absent" || cardState == "unavailable"
            || cardMuted == true
    }

    var identityObservationUnavailable: Bool {
        hasConfirmedCardInterruption || metadataReady == false
    }

    var cardObservationMessage: String? {
        if cardState == "absent" { return "读卡器已连接，但未检测到卡片" }
        if cardMuted == true { return "卡片在位但未响应，请重新插入卡片或读卡器后刷新" }
        if cardUnpowered == true { return "卡片尚未上电，请重新插入卡片或读卡器后刷新" }
        if cardState == "unavailable" { return "读卡器暂不可用，请检查连接后刷新" }
        if presenceError != nil { return "卡片状态暂未确认，请刷新重试" }
        if metadataReady == false { return "读卡器已连接，SIM 身份尚未就绪" }
        return nil
    }

    enum CodingKeys: String, CodingKey {
        case connected
        case hasCard = "has_card"
        case cardType = "card_type"
        case readerName = "reader_name"
        case readerGeneration = "reader_generation"
        case hasUSIMInfo = "has_usim_info"
        case hasESIM = "has_esim"
        case cardState = "card_state"
        case presenceSource = "presence_source"
        case presenceError = "presence_error"
        case cardMuted = "card_muted"
        case cardUnpowered = "card_unpowered"
        case cardReady = "card_ready"
        case sessionReady = "session_ready"
        case metadataReady = "metadata_ready"
    }
}

struct USIMInfo: Decodable {
    let iccid: String
    let imsi: String
    let mcc: String
    let mnc: String
    let gid1: String?
	let smsc: String?
	let cardPhoneNumber: String?
    let operatorName: String?
    let operatorSource: String?
    let privateIdentity: String?
    let publicIdentity: String?
    let homeDomain: String?
    let loadedAt: Date?

	/// 只接受 ISIM 公共身份中的电话号码，禁止把 IMSI 当作手机号显示。
	var phoneNumber: String? {
		if let cardPhoneNumber {
			let value = cardPhoneNumber.trimmingCharacters(in: .whitespacesAndNewlines)
			if !value.isEmpty { return value }
		}
        for value in [publicIdentity, privateIdentity].compactMap({ $0 }) {
            let normalized = value.replacingOccurrences(of: "tel:", with: "", options: [.caseInsensitive])
                .split(separator: "@", maxSplits: 1).first.map(String.init) ?? ""
            let digits = normalized.filter { $0.isNumber || $0 == "+" }
            guard digits.hasPrefix("+") && digits.dropFirst().count >= 7 && digits.dropFirst().count <= 15 else { continue }
            guard !digits.dropFirst().allSatisfy({ $0 == "0" }) else { continue }
            return digits
        }
        return nil
    }

    var operatorDisplayName: String {
        if let operatorName {
            let value = operatorName.trimmingCharacters(in: .whitespacesAndNewlines)
            if !value.isEmpty { return value }
        }
        let code = [mcc, mnc]
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: "/")
        return code.isEmpty ? "未知运营商" : code
    }

    enum CodingKeys: String, CodingKey {
		case iccid, imsi, mcc, mnc, gid1, smsc
		case cardPhoneNumber = "phone_number"
        case operatorName = "operator_name"
        case operatorSource = "operator_source"
        case privateIdentity = "private_identity"
        case publicIdentity = "public_identity"
        case homeDomain = "home_domain"
        case loadedAt = "loaded_at"
    }
}

/// POST /api/reader/connect 请求
struct ReaderConnectRequest: Encodable {
    let clientRequestID: String
    let reader: String
    let readerGeneration: UInt64
    let inventoryRevision: UInt64
    let instanceID: String
    var autoConnect: ReaderAutoConnect? = nil
    enum CodingKeys: String, CodingKey {
        case reader
        case clientRequestID = "client_request_id"
        case readerGeneration = "reader_generation"
        case inventoryRevision = "inventory_revision"
        case instanceID = "instance_id"
        case autoConnect = "auto_connect"
    }
}

/// POST /api/reader/disconnect request.  The target identity is retained in
/// the durable checkpoint so a lost reply cannot later disconnect a successor
/// reader selected under a newer generation.
struct ReaderDisconnectRequest: Encodable {
    let clientRequestID: String
    let reader: String
    let readerGeneration: UInt64
    let inventoryRevision: UInt64
    let instanceID: String

    enum CodingKeys: String, CodingKey {
        case reader
        case clientRequestID = "client_request_id"
        case readerGeneration = "reader_generation"
        case inventoryRevision = "inventory_revision"
        case instanceID = "instance_id"
    }
}

struct ReaderLifecycleMutationCheckpoint: Codable, Equatable {
    let action: String
    let clientRequestID: String
    let reader: String
    let readerGeneration: UInt64
    let inventoryRevision: UInt64
    let instanceID: String
    let autoConnect: ReaderAutoConnect?

    func hasSameIntent(
        action: String,
        reader: String,
        readerGeneration: UInt64,
        inventoryRevision: UInt64,
        instanceID: String,
        autoConnect: ReaderAutoConnect?
    ) -> Bool {
        self.action == action && self.reader == reader
            && self.readerGeneration == readerGeneration
            && self.inventoryRevision == inventoryRevision
            && self.instanceID == instanceID && self.autoConnect == autoConnect
    }
}

enum ReaderLifecycleCheckpointLoad: Equatable {
    case missing
    case valid(ReaderLifecycleMutationCheckpoint)
    case corrupt(String)
}

protocol ReaderLifecycleCheckpointDefaults: AnyObject {
    func object(forKey defaultName: String) -> Any?
    func set(_ value: Any?, forKey defaultName: String)
    func removeObject(forKey defaultName: String)
    func synchronize() -> Bool
}

extension UserDefaults: ReaderLifecycleCheckpointDefaults {}

/// One durable public reader lifecycle intent.  A failed write/clear remains
/// a sticky process-local fence even if UserDefaults' cache appears to contain
/// the desired value.  Existing non-Data/corrupt evidence is never replaced.
final class ReaderLifecycleMutationCheckpointStore {
    private let defaults: ReaderLifecycleCheckpointDefaults
    private let key: String
    private var persistenceFailure: String?

    init(
        defaults: ReaderLifecycleCheckpointDefaults = UserDefaults.standard,
        key: String? = nil
    ) {
        self.defaults = defaults
        let owner = Bundle.main.bundleIdentifier
            ?? "cli.\(ProcessInfo.processInfo.processIdentifier).\(UUID().uuidString.lowercased())"
        self.key = key ?? "switch.reader.lifecycle.mutation.v1.\(owner)"
    }

    func load() -> ReaderLifecycleCheckpointLoad {
        if let persistenceFailure { return .corrupt(persistenceFailure) }
        guard let stored = defaults.object(forKey: key) else { return .missing }
        guard let data = stored as? Data else {
            return .corrupt("reader lifecycle checkpoint is not Data")
        }
        guard let value = try? JSONDecoder().decode(
            ReaderLifecycleMutationCheckpoint.self, from: data),
              Self.valid(value) else {
            return .corrupt("reader lifecycle checkpoint is invalid")
        }
        return .valid(value)
    }

    func begin(
        action: String,
        reader: String,
        readerGeneration: UInt64,
        inventoryRevision: UInt64,
        instanceID: String,
        autoConnect: ReaderAutoConnect?
    ) -> ReaderLifecycleMutationCheckpoint? {
        switch load() {
        case .valid(let existing):
            guard existing.hasSameIntent(
                action: action, reader: reader,
                readerGeneration: readerGeneration,
                inventoryRevision: inventoryRevision,
                instanceID: instanceID, autoConnect: autoConnect),
                  confirmStored(existing) else { return nil }
            return existing
        case .corrupt:
            return nil
        case .missing:
            break
        }
        let value = ReaderLifecycleMutationCheckpoint(
            action: action,
            clientRequestID: UUID().uuidString.lowercased(),
            reader: reader,
            readerGeneration: readerGeneration,
            inventoryRevision: inventoryRevision,
            instanceID: instanceID,
            autoConnect: autoConnect)
        guard Self.valid(value), let data = try? JSONEncoder().encode(value) else { return nil }
        defaults.set(data, forKey: key)
        guard confirmStored(value) else { return nil }
        return value
    }

    @discardableResult
    func clear(_ checkpoint: ReaderLifecycleMutationCheckpoint) -> Bool {
        guard load() == .valid(checkpoint),
              let retained = defaults.object(forKey: key) as? Data else { return false }
        defaults.removeObject(forKey: key)
        guard defaults.synchronize(), load() == .missing else {
            defaults.set(retained, forKey: key)
            _ = defaults.synchronize()
            persistenceFailure = "reader lifecycle checkpoint clear synchronize/readback failed"
            return false
        }
        return true
    }

    private func confirmStored(_ checkpoint: ReaderLifecycleMutationCheckpoint) -> Bool {
        guard persistenceFailure == nil,
              let retained = defaults.object(forKey: key) as? Data,
              defaults.synchronize(),
              defaults.object(forKey: key) as? Data == retained,
              load() == .valid(checkpoint) else {
            persistenceFailure = "reader lifecycle checkpoint save synchronize/readback failed"
            return false
        }
        return true
    }

    private static func valid(_ value: ReaderLifecycleMutationCheckpoint) -> Bool {
        ["connect", "disconnect"].contains(value.action)
            && validUUID(value.clientRequestID)
            && !value.reader.isEmpty && value.reader == value.reader.trimmingCharacters(in: .whitespacesAndNewlines)
            && value.reader.utf8.count <= 512 && !value.reader.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
            && value.inventoryRevision > 0
            && !value.instanceID.isEmpty && value.instanceID == value.instanceID.trimmingCharacters(in: .whitespacesAndNewlines)
            && value.instanceID.utf8.count <= 128
            && (value.autoConnect == nil || value.autoConnect == ReaderAutoConnect(
                reader: value.reader,
                inventoryRevision: value.inventoryRevision,
                readerGeneration: value.readerGeneration,
                instanceID: value.instanceID))
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

/// POST /api/reader/auth 请求
struct ReaderAuthRequest: Encodable {
    let rand: String
    let autn: String
    let mode: String
}

/// POST /api/reader/auth 响应
struct ReaderAuthResponse: Decodable {
    let res: String
    let ck: String
    let ik: String
    let auts: String?
}

/// GET /api/reader/vowifi 响应 — 读卡器模式 VoWiFi 状态
struct ReaderVoWiFiStatus: Decodable {
	let readerID: String?
	let simID: String?
    let readerGeneration: UInt64?
    let available: Bool
    let mcc: String?
    let mnc: String?
    let imsi: String?
    let imei: String?
    let epdgFqdn: String?
	let epdgSource: String?
	let profileID: String?
	let profileName: String?
	let profileSource: String?
    let tunnelState: String
    let imsState: String?
    let message: String?
    let localIPv4: String?
    let localIPv6: String?
    let pcscf: [String]?
    let ikeEncryption: String?
    let espEncryption: String?
    let registeredMSISDN: String?
    let imsTransport: String?
    let imsSecurityMode: String?
    let smsReady: Bool?
    let smsState: String?
    let callReady: Bool?
    let callState: String?
    let progressStep: Int
    let progressDesc: String?
    let operation: VoWiFiOperation?

    enum CodingKeys: String, CodingKey {
		case readerID = "reader_id"
		case simID = "sim_id"
        case readerGeneration = "reader_generation"
        case available, operation
        case mcc, mnc, imsi, imei
        case epdgFqdn = "epdg_fqdn"
		case epdgSource = "epdg_source"
		case profileID = "profile_id"
		case profileName = "profile_name"
		case profileSource = "profile_source"
        case tunnelState = "tunnel_state"
        case imsState = "ims_state"
        case message
        case localIPv4 = "local_ipv4"
        case localIPv6 = "local_ipv6"
        case pcscf
        case ikeEncryption = "ike_encryption"
        case espEncryption = "esp_encryption"
        case registeredMSISDN = "registered_msisdn"
        case imsTransport = "ims_transport"
        case imsSecurityMode = "ims_security_mode"
        case smsReady = "sms_ready"
        case smsState = "sms_state"
        case callReady = "call_ready"
        case callState = "call_state"
        case progressStep = "progress_step"
        case progressDesc = "progress_desc"
    }

    /// 从后端 init 补全默认值（JSON decode 时 progressStep 可能缺失）
    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
		readerID = try c.decodeIfPresent(String.self, forKey: .readerID)
		simID = try c.decodeIfPresent(String.self, forKey: .simID)
        readerGeneration = try c.decodeIfPresent(UInt64.self, forKey: .readerGeneration)
        available = try c.decode(Bool.self, forKey: .available)
        mcc = try c.decodeIfPresent(String.self, forKey: .mcc)
        mnc = try c.decodeIfPresent(String.self, forKey: .mnc)
        imsi = try c.decodeIfPresent(String.self, forKey: .imsi)
        imei = try c.decodeIfPresent(String.self, forKey: .imei)
        epdgFqdn = try c.decodeIfPresent(String.self, forKey: .epdgFqdn)
        epdgSource = try c.decodeIfPresent(String.self, forKey: .epdgSource)
        profileID = try c.decodeIfPresent(String.self, forKey: .profileID)
        profileName = try c.decodeIfPresent(String.self, forKey: .profileName)
        profileSource = try c.decodeIfPresent(String.self, forKey: .profileSource)
        tunnelState = try c.decode(String.self, forKey: .tunnelState)
        imsState = try c.decodeIfPresent(String.self, forKey: .imsState)
        message = try c.decodeIfPresent(String.self, forKey: .message)
        localIPv4 = try c.decodeIfPresent(String.self, forKey: .localIPv4)
        localIPv6 = try c.decodeIfPresent(String.self, forKey: .localIPv6)
        pcscf = try c.decodeIfPresent([String].self, forKey: .pcscf)
        ikeEncryption = try c.decodeIfPresent(String.self, forKey: .ikeEncryption)
        espEncryption = try c.decodeIfPresent(String.self, forKey: .espEncryption)
        registeredMSISDN = try c.decodeIfPresent(String.self, forKey: .registeredMSISDN)
        imsTransport = try c.decodeIfPresent(String.self, forKey: .imsTransport)
        imsSecurityMode = try c.decodeIfPresent(String.self, forKey: .imsSecurityMode)
        smsReady = try c.decodeIfPresent(Bool.self, forKey: .smsReady)
        smsState = try c.decodeIfPresent(String.self, forKey: .smsState)
        callReady = try c.decodeIfPresent(Bool.self, forKey: .callReady)
        callState = try c.decodeIfPresent(String.self, forKey: .callState)
        progressStep = try c.decodeIfPresent(Int.self, forKey: .progressStep) ?? 0
        progressDesc = try c.decodeIfPresent(String.self, forKey: .progressDesc) ?? ""
        operation = try c.decodeIfPresent(VoWiFiOperation.self, forKey: .operation)
    }
}

struct ReaderVoWiFiSessionsResponse: Decodable {
	let sessions: [ReaderVoWiFiStatus]
}

/// POST /api/reader/vowifi 请求
struct ReaderVoWiFiRequest: Encodable {
    let enable: Bool
    let proxy: String?
    let proxyHost: String?
    let proxyPort: String?
    let epdg: String?
    let epdgPort: Int?
    let epdgPortText: String?
    let observeOperation: Bool?
    let operationID: String?
    let operationGeneration: UInt64?
    let clientRequestID: String?

    enum CodingKeys: String, CodingKey {
        case enable, proxy, epdg
        case proxyHost = "proxy_host"
        case proxyPort = "proxy_port"
        case epdgPort = "epdg_port"
        case epdgPortText = "epdg_port_text"
        case observeOperation = "observe_operation"
        case operationID = "operation_id"
        case operationGeneration = "operation_generation"
        case clientRequestID = "client_request_id"
    }

    init(enable: Bool, proxy: String?, proxyHost: String? = nil, proxyPort: String? = nil,
         epdg: String? = nil, epdgPort: Int? = nil, epdgPortText: String? = nil,
         observeOperation: Bool? = nil, operation: VoWiFiOperation? = nil,
         clientRequestID: String? = nil) {
        self.enable = enable
        self.proxy = proxy
        self.proxyHost = proxyHost
        self.proxyPort = proxyPort
        self.epdg = epdg
        self.epdgPort = epdgPort
        self.epdgPortText = epdgPortText
        self.observeOperation = observeOperation
        self.operationID = operation?.operationID
        self.operationGeneration = operation?.generation
        self.clientRequestID = clientRequestID
    }
}

/// POST /api/reader/vowifi/proxy-check 请求
struct ProxyCheckRequest: Encodable {
    let proxyHost: String
    let proxyPort: String
    /// Raw user override. Empty means Rust chooses from the projected facts.
    let epdg: String?
    /// Read-only facts already projected by Rust. Swift transports both and
    /// never decides which one is the effective diagnostic destination.
    let sessionEPDG: String?
    let readerEPDG: String?

    enum CodingKeys: String, CodingKey {
        case proxyHost = "proxy_host"
        case proxyPort = "proxy_port"
        case epdg
        case sessionEPDG = "session_epdg"
        case readerEPDG = "reader_epdg"
    }

    init(proxyHost: String, proxyPort: String, epdg: String?,
         sessionEPDG: String? = nil, readerEPDG: String? = nil) {
        self.proxyHost = proxyHost
        self.proxyPort = proxyPort
        self.epdg = epdg
        self.sessionEPDG = sessionEPDG
        self.readerEPDG = readerEPDG
    }
}

/// POST /api/reader/vowifi/proxy-check 响应
struct ProxyCheckResult: Decodable {
    let ok: Bool
    let probeMessage: String?
    let countryCode: String?
    let country: String?
    let exitIP: String?
    let latencyMs: Int64?
    let udpReady: Bool?
    let transport: String?
    let udpMessage: String?
    let effectiveEPDG: String?
    let epdgSource: String?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case ok, country, transport, error
        case probeMessage = "probe_message"
        case countryCode = "country_code"
        case exitIP = "exit_ip"
        case latencyMs = "latency_ms"
        case udpReady = "udp_ready"
        case udpMessage = "udp_message"
        case effectiveEPDG = "effective_epdg"
        case epdgSource = "epdg_source"
    }

    /// 国旗 emoji
    var countryFlag: String? {
        guard let cc = countryCode, !cc.isEmpty else { return nil }
        let emoji = flagEmoji(countryCode: cc)
        return emoji.isEmpty ? nil : emoji
    }
}
