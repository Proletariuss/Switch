import Foundation

enum DiagnosticOperation: String, Codable, CaseIterable {
    case imsRegister = "ims_register", nwaitSession = "nwait_session"
    case networkDiagnostic = "network_diagnostic", readerAuth = "reader_auth"
    case smsSend = "sms_send", smsForward = "sms_forward"
    var label: String {
        switch self {
        case .imsRegister: return "IMS 注册"
        case .nwaitSession: return "N待会话"
        case .networkDiagnostic: return "网络采集"
        case .readerAuth: return "读卡器认证"
        case .smsSend: return "短信提交"
        case .smsForward: return "短信转发"
        }
    }
}

enum DiagnosticStage: String, Codable {
    case queued, waitingLock = "waiting_lock", prepare, connect, authenticate, ready, submit, retire, complete
    var label: String {
        switch self {
        case .queued: return "排队"
        case .waitingLock: return "等待许可"
        case .prepare: return "准备"
        case .connect: return "连接"
        case .authenticate: return "认证"
        case .ready: return "就绪"
        case .submit: return "提交"
        case .retire: return "退役"
        case .complete: return "完成"
        }
    }
}

enum DiagnosticEventCode: String, Codable {
    case started, ready, success, cancelled, busy, timeout, stopped, unavailable
    case transportFailed = "transport_failed", authenticationFailed = "authentication_failed"
    case registrationRejected = "registration_rejected", bindingRevoked = "binding_revoked"
    case bindingUnconfirmed = "binding_unconfirmed", bindingAmbiguous = "binding_ambiguous"
    case submissionFailed = "submission_failed", resultUnknown = "result_unknown"
    case invalidResponse = "invalid_response", plannedRenewal = "planned_renewal"
    case plannedIKEReauth = "planned_ike_reauth", manualRetry = "manual_retry"
    var label: String {
        switch self {
        case .started: return "已开始"
        case .ready: return "已就绪"
        case .success: return "已完成"
        case .cancelled: return "已取消"
        case .busy: return "占用中"
        case .timeout: return "超时"
        case .stopped: return "已停止"
        case .unavailable: return "暂不可用"
        case .transportFailed: return "链路未完成"
        case .authenticationFailed: return "认证未完成"
        case .registrationRejected: return "注册未确认"
        case .bindingRevoked: return "绑定已撤销"
        case .bindingUnconfirmed: return "绑定未确认"
        case .bindingAmbiguous: return "绑定有歧义"
        case .submissionFailed: return "提交未完成"
        case .resultUnknown: return "结果待确认"
        case .invalidResponse: return "响应无法确认"
        case .plannedRenewal: return "计划 IMS 续期"
        case .plannedIKEReauth: return "计划 IKE 重认证"
        case .manualRetry: return "手动重连"
        }
    }
}

enum DiagnosticEventResult: String, Codable {
    case pending, succeeded, failed, cancelled, unknown
    var label: String {
        switch self {
        case .pending: return "处理中"
        case .succeeded: return "完成"
        case .failed: return "未完成"
        case .cancelled: return "取消"
        case .unknown: return "待确认"
        }
    }
}

/// Closed shareable DTO. Unknown strings, raw logs and response extras cannot
/// be copied into the export; decoding errors are never shown verbatim.
struct DiagnosticOperationEvent: Codable, Equatable, Identifiable {
    let sequence: UInt64
    let timeMS: Int64
    let operation: DiagnosticOperation
    let correlationID: String
    let generation: UInt64
    let stage: DiagnosticStage
    let code: DiagnosticEventCode
    let durationMS: Int64
    let result: DiagnosticEventResult
    var id: String { correlationID + ":" + String(sequence) }
    var date: Date { Date(timeIntervalSince1970: Double(timeMS) / 1_000) }
    enum CodingKeys: String, CodingKey {
        case sequence, timeMS = "time_ms", operation, correlationID = "correlation_id"
        case generation, stage, code, durationMS = "duration_ms", result
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sequence = try c.decode(UInt64.self, forKey: .sequence)
        timeMS = try c.decode(Int64.self, forKey: .timeMS)
        operation = try c.decode(DiagnosticOperation.self, forKey: .operation)
        correlationID = try c.decode(String.self, forKey: .correlationID)
        generation = try c.decode(UInt64.self, forKey: .generation)
        stage = try c.decode(DiagnosticStage.self, forKey: .stage)
        code = try c.decode(DiagnosticEventCode.self, forKey: .code)
        durationMS = try c.decode(Int64.self, forKey: .durationMS)
        result = try c.decode(DiagnosticEventResult.self, forKey: .result)
        guard sequence > 0, timeMS >= 0, durationMS >= 0,
              correlationID.utf8.count == 32,
              correlationID.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Invalid safe event metadata"))
        }
    }
}

struct DiagnosticEventSnapshot: Codable, Equatable {
    let schemaVersion: Int
    let capacity: Int
    let latestSequence: UInt64
    let events: [DiagnosticOperationEvent]
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", capacity, latestSequence = "latest_sequence", events
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        capacity = try c.decode(Int.self, forKey: .capacity)
        latestSequence = try c.decode(UInt64.self, forKey: .latestSequence)
        events = try c.decode([DiagnosticOperationEvent].self, forKey: .events)
        let sequences = events.map(\.sequence)
        guard schemaVersion == 1, (1...512).contains(capacity), events.count <= capacity,
              sequences == sequences.sorted(), Set(sequences).count == sequences.count,
              latestSequence == (events.last?.sequence ?? 0) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Invalid safe event snapshot"))
        }
    }
    func exportData() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }
}
