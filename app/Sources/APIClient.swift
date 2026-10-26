import Foundation

/// 基于 URLSession + UnixSocketURLProtocol 的 API 客户端。
/// 请求走 http+unix:// 协议，socket 路径由 BackendProcess 统一提供。
struct APIClient {
    private static let base = URL(string: "http+unix://proletarius")!

    private let session: URLSession
    private let timeoutInterval: TimeInterval
    private let encoder = JSONEncoder()

    init(timeoutInterval: TimeInterval = 30) {
        self.timeoutInterval = timeoutInterval
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [UnixSocketURLProtocol.self]
        config.timeoutIntervalForRequest = timeoutInterval
        config.timeoutIntervalForResource = max(60, timeoutInterval)
        session = URLSession(configuration: config)

        encoder.dateEncodingStrategy = .iso8601
    }

    /// 每个响应独立解码，formatter 只在此次同步 decode 内复用，不跨并发请求共享。
    static func decodeResponse<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        let decoder = JSONDecoder()
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]

        // Go 端 time.Time 输出 RFC3339Nano，小数秒位数不固定（1-9 位）
        // 优先解析原始值；格式化器不接受小数秒精度时，保留截断到 3 位的回退。
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let str = try container.decode(String.self)
            if let d = withFraction.date(from: str) { return d }
            if let d = plain.date(from: str) { return d }
            // Go 的 RFC3339Nano 可能输出 6-9 位小数秒，截断为 3 位再解析
            if let dotRange = str.range(of: ".") {
                let beforeFraction = String(str[..<dotRange.lowerBound])
                let afterDot = String(str[dotRange.upperBound...])
                // 提取时区部分（Z 或 +xx:xx / -xx:xx）
                let tzStart: String.Index
                if let zIdx = afterDot.firstIndex(of: "Z") {
                    tzStart = zIdx
                } else if let plusIdx = afterDot.range(of: #"[\+\-]\d{2}:\d{2}$"#, options: .regularExpression)?.lowerBound {
                    tzStart = plusIdx
                } else {
                    throw DecodingError.dataCorruptedError(
                        in: container, debugDescription: "无法解析时间：\(str)")
                }
                let fractionDigits = String(afterDot[..<tzStart])
                let tzPart = String(afterDot[tzStart...])
                let truncatedFraction = String(fractionDigits.prefix(3))
                let rebuilt = beforeFraction + "." + truncatedFraction + tzPart
                if let d = withFraction.date(from: rebuilt) { return d }
            }
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "无法解析时间：\(str)")
        }

        return try decoder.decode(type, from: data)
    }

    func get<T: Decodable>(_ path: String, as type: T.Type = T.self) async throws -> T {
        let url = Self.makeURL(path)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        Self.authorize(&request)
        let (data, response) = try await performRead(request)
        try Self.validate(response, data: data)
        return try Self.decodeResponse(T.self, from: data)
    }

    func getData(_ path: String) async throws -> Data {
        let url = Self.makeURL(path)
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        Self.authorize(&request)
        let (data, response) = try await performRead(request)
        try Self.validate(response, data: data)
        return data
    }

    func upload<T: Decodable>(
        _ path: String,
        data: Data,
        contentType: String = "application/json",
        as type: T.Type = T.self
    ) async throws -> T {
        let url = Self.makeURL(path)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        Self.authorize(&request)
        request.httpBody = data
        request.setValue(contentType, forHTTPHeaderField: "Content-Type")
        let (responseData, response) = try await performMutation(request)
        try Self.validate(response, data: responseData)
        do {
            let value = try Self.decodeResponse(T.self, from: responseData)
            try PublicRadioMutationCheckpoint.completeResponse(path: path, data: responseData)
            return value
        } catch {
            // The daemon returned 2xx, so the upload may already be committed.
            // Keep the result unknown instead of inviting the caller to replay it.
            throw APIOperationOutcomeUnknownError(reason: "本机服务已响应，但无法解析操作结果")
        }
    }

    func send<T: Decodable>(
        _ path: String,
        method: String = "POST",
        body: (any Encodable)? = nil,
        as type: T.Type = T.self
    ) async throws -> T {
        let url = Self.makeURL(path)
        var request = URLRequest(url: url)
        request.httpMethod = method
        Self.authorize(&request)
        if let body {
            request.httpBody = try encoder.encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await performMutation(request)
        try Self.validate(response, data: data)
        do {
            let value = try Self.decodeResponse(T.self, from: data)
            try PublicRadioMutationCheckpoint.completeResponse(path: path, data: data)
            return value
        } catch {
            // A 2xx write response may arrive after the operation committed.
            // A malformed payload must not encourage sending the write again.
            throw APIOperationOutcomeUnknownError(reason: "本机服务已响应，但无法解析操作结果")
        }
    }

    func send(_ path: String, method: String = "POST", body: (any Encodable)? = nil) async throws {
        let url = Self.makeURL(path)
        var request = URLRequest(url: url)
        request.httpMethod = method
        Self.authorize(&request)
        if let body {
            request.httpBody = try encoder.encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await performMutation(request)
        try Self.validate(response, data: data)
        try PublicRadioMutationCheckpoint.completeResponse(path: path, data: data)
    }

    static func readTimeout(path: String, defaultTimeout: TimeInterval) -> TimeInterval {
        let clean = "/" + (path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first ?? "")
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if clean == "/api/module/flight-mode" { return max(100, defaultTimeout) }
        return clean == "/api/sms/storage" ? max(180, defaultTimeout) : defaultTimeout
    }

    // Storage usage restores the device's previous selected memory before
    // acknowledging. This exact GET needs the transaction budget, but remains
    // a read error (never a mutation-outcome-unknown classification).
    private func performRead(_ original: URLRequest) async throws -> (Data, URLResponse) {
        var request = original
        let timeout = Self.readTimeout(path: request.url?.path ?? "", defaultTimeout: timeoutInterval)
        request.timeoutInterval = timeout
        guard timeout > session.configuration.timeoutIntervalForResource else {
            return try await observedData(for: request, using: session)
        }
        let configuration = session.configuration
        configuration.timeoutIntervalForRequest = timeout
        configuration.timeoutIntervalForResource = timeout
        let readSession = URLSession(configuration: configuration)
        defer { readSession.finishTasksAndInvalidate() }
        return try await observedData(for: request, using: readSession)
    }

    private func performMutation(_ original: URLRequest) async throws -> (Data, URLResponse) {
        var request = original
        _ = try PublicRadioMutationCheckpoint.prepare(&request)
        // URLSession can replace finite Data with httpBodyStream. Preserve the
        // exact post-checkpoint bytes; the transport never reads an arbitrary stream.
        if let body = request.httpBody {
            let preserved = (request as NSURLRequest).mutableCopy() as! NSMutableURLRequest
            URLProtocol.setProperty(body, forKey: "Switch.FiniteHTTPBody.v1", in: preserved)
            request = preserved as URLRequest
        }
        // This is a transport waiter budget only. Business deadlines, retry
        // admission, and terminal outcomes belong to the durable Rust owner and
        // must never be inferred from a Swift route-name table.
        request.timeoutInterval = timeoutInterval
        do {
            let result = try await observedData(for: request, using: session)
            if let http=result.1 as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                // A durable terminal admission error (for example busy) settles
                // this caller ID too. Unknown receipts retain their checkpoint.
                try PublicRadioMutationCheckpoint.completeResponse(path: request.url?.path ?? "", data: result.0)
            }
            try Self.validateESIMMutationAcceptedResponse(result.1, data: result.0, request: request)
            return result
        } catch let error as URLError
            where [.timedOut, .networkConnectionLost, .badServerResponse, .cancelled].contains(error.code) {
            throw APIOperationOutcomeUnknownError()
        }
    }

    /// Switch/profile results have optional-only DTOs, so no 202 can prove
    /// success on those routes. Download can return a real operation snapshot;
    /// distinguish it from Rust's unresolved intent receipt before decoding.
    private static func validateESIMMutationAcceptedResponse(
        _ response: URLResponse, data: Data, request: URLRequest
    ) throws {
        guard let http = response as? HTTPURLResponse, http.statusCode == 202 else { return }
        let route = (request.httpMethod ?? "", request.url?.path ?? "")
        switch route {
        case ("POST", "/api/esim/switch"), ("DELETE", "/api/esim/profile"),
             ("PATCH", "/api/esim/profile"),
             ("POST", "/api/reader/esim/switch"), ("DELETE", "/api/reader/esim/profile"),
             ("PATCH", "/api/reader/esim/profile"):
            throw APIOperationOutcomeUnknownError(reason: "eSIM 原请求返回 HTTP 202，操作结果仍待确认")
        case ("POST", "/api/esim/download"), ("POST", "/api/reader/esim/download"):
            if let receipt = try? JSONDecoder().decode(ESIMMutationAdmissionReceipt.self, from: data) {
                throw APIOperationOutcomeUnknownError(
                    reason: "eSIM 请求 \(receipt.clientRequestID) 已受理，原回执仍为 \(receipt.phase)")
            }
            guard let operation = try? Self.decodeResponse(ESIMAcceptedDownloadOperation.self, from: data),
                  !operation.id.isEmpty, operation.kind == "download_profile",
                  ["queued", "running", "awaiting_confirmation"].contains(operation.state) else {
                throw APIOperationOutcomeUnknownError(reason: "eSIM 下载返回 HTTP 202，但操作回执无法确认")
            }
        default:
            return
        }
    }

    private static func validate(_ response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse,
              !(200..<300).contains(http.statusCode) else { return }
        let failure: APIError
        if let payload = try? JSONDecoder().decode(APIErrorPayload.self, from: data),
           !payload.error.isEmpty {
            if let refresh = payload.refresh {
                failure = .esimRefresh(http.statusCode, code: payload.code,
                                       message: payload.error, outcome: refresh)
            } else {
                failure = .server(http.statusCode, code: payload.code,
                                  message: payload.error, recoverable: payload.recoverable ?? false)
            }
        } else { failure = .httpStatus(http.statusCode) }
        // Only a durable Rust finish/replay emits these headers. A recovery
        // caller must still match the original request ID and preserve unknowns.
        if http.value(forHTTPHeaderField: "X-Switch-Call-Terminal") == "true",
           let id = http.value(forHTTPHeaderField: "X-Switch-Call-Request-ID"),
           !id.isEmpty, id.utf8.count <= 128,
           id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0)
               || (97...122).contains($0) || $0 == 45 || $0 == 95 }) {
            throw APICallTerminalError(clientRequestID: id, underlying: failure)
        }
        throw failure
    }

    private static func authorize(_ request: inout URLRequest) {
        request.setValue(
            LocalBackendAuthentication.appToken,
            forHTTPHeaderField: LocalBackendAuthentication.headerName)
    }


    private func observedData(for original: URLRequest, using session: URLSession) async throws -> (Data, URLResponse) {
        var request = original
        let correlation = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        request.setValue(correlation, forHTTPHeaderField: "X-Switch-Diagnostic-ID")
        let start = ProcessInfo.processInfo.systemUptime
        let route = request.url?.path ?? ""
        let excluded = route.hasPrefix("/api/diagnostics/")
        let safeRoutes: Set<String> = ["/api/reader/vowifi", "/api/reader/vowifi/reconcile", "/api/reader/vowifi/stop-control", "/api/vowifi", "/api/vowifi/result", "/api/vowifi/reconcile", "/api/vowifi/stop-control", "/api/call/dial", "/api/call/answer", "/api/call/hangup", "/api/call/status", "/api/reader/vowifi/calls/dial", "/api/reader/vowifi/calls/answer", "/api/reader/vowifi/calls/hangup", "/api/reader/vowifi/calls/reject", "/api/reader/vowifi/calls/status", "/api/vowifi/calls/dial", "/api/vowifi/calls/answer", "/api/vowifi/calls/hangup", "/api/vowifi/calls/reject", "/api/vowifi/calls/status", "/api/sms/send", "/api/sms/storage", "/api/sms/forwarding/status", "/api/sms/forwarding/configure", "/api/sms/forwarding/test", "/api/network/services", "/api/network/services-order", "/api/network/usbnet", "/api/status", "/api/reader/status", "/api/multistandby/status"]
        let safeRoute = safeRoutes.contains(route) ? route : "/api/other"
        let method = ["GET","POST","PUT","PATCH","DELETE","HEAD"].contains(request.httpMethod ?? "") ? request.httpMethod! : "OTHER"
        var originalID: String?
        if let data = request.httpBody, data.count <= 32768,
           let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let id = object["client_request_id"] as? String, let uuid = UUID(uuidString: id) {
            originalID = uuid.uuidString.lowercased()
        }
        func record(_ stage: String, status: Int = 0, error: Error? = nil) {
            guard !excluded else { return }
            let failure = error as? URLError
            var values: [String: Any] = [
                "schema_version": 1, "time_ms": Int64(Date().timeIntervalSince1970 * 1000),
                "correlation_id": correlation, "route": safeRoute, "method": method,
                "stage": stage, "status": status, "error_domain": failure == nil ? "none" : "url",
                "error_code": failure?.errorCode ?? 0,
                "duration_ms": Int64(max(0, ProcessInfo.processInfo.systemUptime - start) * 1000)
            ]
            if let originalID { values["request_id"] = originalID }
            NotificationCenter.default.post(name: Notification.Name("Switch.SafeAPIDiagnostic.v1"),
                                            object: nil, userInfo: values)
        }
        record("started")
        do {
            let result = try await session.data(for: request)
            record("response", status: (result.1 as? HTTPURLResponse)?.statusCode ?? 0)
            return result
        } catch {
            record("transport_failed", error: error)
            throw error
        }
    }

    private static func makeURL(_ path: String) -> URL {
        let components = path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        let cleanPath = String(components[0])
        let url = base.appendingPathComponent(cleanPath)
        guard components.count == 2,
              var urlComponents = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            return url
        }
        urlComponents.percentEncodedQuery = String(components[1])
        return urlComponents.url ?? url
    }
}

private struct ESIMMutationAdmissionReceipt: Decodable {
    let clientRequestID: String
    let scope: String
    let phase: String

    enum CodingKeys: String, CodingKey {
        case scope, phase
        case clientRequestID = "client_request_id"
    }
}

private struct ESIMAcceptedDownloadOperation: Decodable {
    let id: String
    let kind: String
    let state: String
    let progress: Int
    let startedAt: Date
    let updatedAt: Date

    enum CodingKeys: String, CodingKey {
        case id, kind, state, progress
        case startedAt = "started_at"
        case updatedAt = "updated_at"
    }
}

struct APICallTerminalError: LocalizedError {
    let clientRequestID: String
    let underlying: APIError
    var errorDescription: String? { underlying.localizedDescription }
}

struct APIOperationOutcomeUnknownError: LocalizedError {
    var reason = "等待本机服务响应时超时或连接中断"
    var errorDescription: String? {
        "操作结果待确认：\(reason)。请刷新操作状态或发送记录，确认前不要重复提交。"
    }
}

func operationOutcomeIsUnknown(_ error: any Error) -> Bool {
    if let terminal = error as? APICallTerminalError { return operationOutcomeIsUnknown(terminal.underlying) }
    if error is APIOperationOutcomeUnknownError { return true }
    if let apiError = error as? APIError,
       apiError.code?.hasSuffix("outcome_unknown") == true
        || apiError.code == "esim_mutation_receipt_unavailable"
        || apiError.code == "esim_mutation_dispatch_already_claimed"
        || apiError.code == "esim_mutation_request_id_conflict" { return true }
    return error.localizedDescription.contains("结果待确认")
}

/// A transport waiter ending cannot release a mutation fence when the daemon
/// may already have admitted the operation. Stores use this shared predicate
/// and reconcile the durable Rust status instead of replaying the mutation.
func mutationFailureRequiresStatusReconciliation(_ error: any Error) -> Bool {
    operationOutcomeIsUnknown(error)
        || error is CancellationError
        || (error as? URLError)?.code == .cancelled
}

func smsSendErrorMessage(_ error: any Error) -> String {
    // Cancelling a waiter or replacing the backend cannot prove whether the
    // original request crossed the submit boundary. Recovery owns that answer.
    if error is CancellationError || (error as? URLError)?.code == .cancelled {
        return "发送确认已中断，请查询原发送记录；再次发送可能重复。"
    }
    return operationOutcomeIsUnknown(error) ? userFacingErrorMessage(error) : "发送失败：\(userFacingErrorMessage(error))"
}

private struct APIErrorPayload: Decodable {
    let error: String
    let code: String?
    let recoverable: Bool?
    let refresh: ESIMRefreshOutcome?
}

enum APIError: LocalizedError, ESIMRefreshFailure {
    case httpStatus(Int)
    case server(Int, code: String?, message: String, recoverable: Bool)
    case esimRefresh(Int, code: String?, message: String, outcome: ESIMRefreshOutcome)

    var refreshOutcome: ESIMRefreshOutcome? {
        if case .esimRefresh(_, _, _, let outcome) = self { return outcome }
        return nil
    }

    var statusCode: Int {
        switch self {
        case .httpStatus(let code), .server(let code, _, _, _), .esimRefresh(let code, _, _, _): return code
        }
    }

    var code: String? {
        switch self {
        case .httpStatus: return nil
        case .server(_, let code, _, _): return code
        case .esimRefresh(_, let code, _, _): return code
        }
    }

    var isRecoverable: Bool {
        switch self {
        case .httpStatus: return true
        case .server(_, _, _, let recoverable): return recoverable
        case .esimRefresh: return false
        }
    }

    var errorDescription: String? {
        switch self {
        case .httpStatus(let code): return "服务返回错误（HTTP \(code)）"
        case .server(_, _, let message, _): return localizedBackendMessage(message)
        case .esimRefresh(_, _, _, let outcome): return outcome.message
        }
    }
}

func readerLifecycleFailureWasDefinitelyNotSubmitted(_ error: any Error) -> Bool {
    guard let apiError = error as? APIError else { return false }
    return apiError.statusCode == 409 && apiError.code == "stale_reader_binding"
}

/// 将后端或系统返回的常见技术错误转换为可直接操作的中文提示。
func userFacingErrorMessage(_ error: any Error) -> String {
    if let apiError = error as? APIError {
        return apiError.errorDescription ?? "请求失败，请稍后重试"
    }
    if let urlError = error as? URLError {
        switch urlError.code {
        case .badURL:
            return "本机请求格式无法处理，请导出诊断包；原操作是否完成仍需核对"
        case .timedOut:
            return "请求超时，请检查设备连接后重试"
        case .cannotConnectToHost, .networkConnectionLost, .notConnectedToInternet:
            return "无法连接本机服务，请确认 Switch 后端正在运行"
        case .cancelled:
            return "操作已取消"
        default:
            break
        }
    }
    return localizedBackendMessage(error.localizedDescription)
}

private func localizedBackendMessage(_ message: String) -> String {
    let normalized = message.lowercased()
    switch normalized {
    case "telegram_configuration_invalid":
        return "请填写有效的 Telegram Bot Token 和 Chat ID"
    case "telegram_system_proxy_not_found":
        return "未找到可用的系统代理，请选择自定义代理或直连"
    case "telegram_system_proxy_resolution_failed":
        return "macOS 无法解析 Telegram 的系统代理，请检查系统代理或改用自定义代理"
    case "telegram_proxy_url_invalid":
        return "Telegram 代理地址无效，请检查协议、主机和端口"
    case "telegram_network_error":
        return "Telegram 网络连接失败，请检查代理是否可连接以及 Telegram 网络是否可达"
    case "telegram_api_rejected":
        return "Telegram API 拒绝了请求，请检查 Bot Token、Chat ID 和 Bot 会话权限"
    case "telegram_http_400":
        return "Telegram 拒绝了请求，请检查 Chat ID 格式和 Bot 会话设置"
    case "telegram_http_401":
        return "Telegram 返回 401：Bot Token 无效或已失效"
    case "telegram_http_403":
        return "Telegram 返回 403：请在 Telegram 中向 Bot 发送 /start，并检查会话或群组权限"
    case "telegram_http_429":
        return "Telegram 请求过于频繁，请稍后重试"
    case "telegram_not_enabled":
        return "Telegram 配置尚未生效，请重新保存后再测试"
    case "telegram_response_invalid", "telegram_response_too_large", "telegram_response_read_failed":
        return "Telegram 返回了无法确认的响应，请检查网络后重试"
    case "bark_network_error":
        return "Bark 网络连接失败，请检查服务器地址和网络"
    case "bark_not_enabled":
        return "Bark 配置尚未生效，请重新保存后再测试"
    default:
        break
    }
    if normalized.contains("dji usb device is not connected")
        || normalized.contains("no usb at device found") {
        return "未检测到 DJI 模块，请检查 USB 连接"
    }
    if normalized.contains("at serial port is unavailable")
        || normalized.contains("usb at device is not open") {
        return "DJI 模块短信通道尚未就绪，请稍后重试"
    }
    if normalized.contains("usb at is cooling down") {
        return "DJI 模块正在重新连接，请稍后重试"
    }
    if normalized.contains("phone and message are required") {
        return "请填写收件人号码和短信内容"
    }
    if normalized.contains("sender is required") {
        return "缺少短信联系人号码"
    }
    if normalized.contains("module sms deletion is only available through usb at") {
        return "DJI 模块短信通道未就绪，暂时无法删除模块内短信"
    }
    return message
}

// MARK: - 模块连接状态检测工具

/// 判断错误消息是否表示 DJI 模块未连接
func isModuleDisconnectedError(_ message: String) -> Bool {
    message.localizedCaseInsensitiveContains("not connected")
        || message.localizedCaseInsensitiveContains("USB device")
        || message.localizedCaseInsensitiveContains("未检测到 DJI 模块")
        || message.localizedCaseInsensitiveContains("未连接")
}

/// 模块未连接时的友好提示文案
let moduleDisconnectedFriendlyMessage = "模块未连接，请确认 DJI 模块已通过 USB 连接"

/// A GUI restart or transport error retains the exact caller identity and body.
/// A second user action cannot replace an unresolved intent for the same route.
/// Rust's durable receipt is the only authority for clearing this checkpoint.
enum PublicRadioMutationCheckpoint {
    struct Intent: Codable { let id: String; let original: Data; let request: Data }
    struct Archive: Codable { var pending: [String: Intent] = [:]; var audioGeneration: String? }
    struct Token { let path: String; let id: String }
    static var defaults: UserDefaults = .standard
    private static let key = "publicRadioMutationCheckpoint.v1"
    private static let lock = NSLock()
    private static var failed = false
    private static let routes: Set<String> = [
        "/api/at", "/api/module/flight-mode", "/api/network/usbnet", "/api/esim/module-notes",
        "/api/call-mode/enable", "/api/call-mode/restore", "/api/call/audio/start", "/api/call/audio/stop",
        "/api/sms/forwarding/queue-action", "/api/sms/clear", "/api/sms/delete-sender",
        "/api/calls/clear", "/api/v2/sms-archive/clear", "/api/v2/sms-archive/delete-sender"
    ]
    private static func validID(_ id:String)->Bool {
        (8...128).contains(id.utf8.count) && id.utf8.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45,46,58,95].contains($0) }
    }
    private static func unknown(_ reason: String) -> APIOperationOutcomeUnknownError { APIOperationOutcomeUnknownError(reason: reason) }
    private static func load() throws -> Archive {
        if failed { throw unknown("原操作持久化状态未确认，请保留原请求并重新启动应用") }
        guard let stored = defaults.object(forKey: key) else { return Archive() }
        guard let data = stored as? Data, let archive = try? JSONDecoder().decode(Archive.self, from: data),
              archive.audioGeneration.map(validID) ?? true,
              archive.pending.allSatisfy({ path,intent in
            guard routes.contains(path), validID(intent.id),
                  var original=(try? JSONSerialization.jsonObject(with:intent.original)) as? [String:Any],
                  let submitted=(try? JSONSerialization.jsonObject(with:intent.request)) as? [String:Any],
                  submitted["client_request_id"] as? String == intent.id else {return false}
            original["client_request_id"]=intent.id
            if path == "/api/call/audio/stop" {
                guard let generation=submitted["audio_generation"] as? String, validID(generation) else {return false}
                original["audio_generation"]=generation
            }
            return (try? JSONSerialization.data(withJSONObject:original,options:[.sortedKeys])) == intent.request
        }) else { failed = true; throw unknown("原操作凭据已损坏，未提交新的模块修改") }
        return archive
    }
    private static func save(_ archive: Archive) throws {
        let data = try JSONEncoder().encode(archive)
        defaults.set(data, forKey: key)
        guard defaults.synchronize(), defaults.object(forKey: key) as? Data == data else {
            failed = true; throw unknown("无法持久保存原操作凭据，未提交新的模块修改")
        }
    }
    static func prepare(_ request: inout URLRequest) throws -> Token? {
        guard let path = request.url?.path, routes.contains(path), request.httpMethod != "GET" else { return nil }
        // Only this finite, audited allowlist is query-only. Unknown AT syntax
        // follows the durable mutation path without restricting raw commands.
        if path == "/api/at", let bytes = request.httpBody,
           let object = (try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any],
           let command = object["command"] as? String,
           ["AT", "ATI", "AT+CGMI", "AT+CGMM", "AT+CGMR", "AT+CGSN", "AT+CSQ", "AT+CESQ", "AT+COPS?", "AT+CPIN?", "AT+CNUM", "AT+CFUN?", "AT+CREG?", "AT+CGREG?", "AT+CEREG?", "AT+QCFG=\"USBNET\"", "AT+QCFG=\"USBCFG\""].contains(command.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()) { return nil }
        lock.lock(); defer { lock.unlock() }
        var archive = try load()
        let object = try JSONSerialization.jsonObject(with: request.httpBody ?? Data("{}".utf8))
        guard var fields = object as? [String: Any] else { throw unknown("模块修改必须携带 JSON 请求对象") }
        let original = try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
        if let pending = archive.pending[path] {
            guard pending.original == original else { throw unknown("此操作仍有未确认的原请求，请先核对原操作结果") }
            // Reconfirm durability even after a prior synchronize failure.
            try save(archive)
            request.httpBody = pending.request
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            return Token(path: path, id: pending.id)
        }
        let id = UUID().uuidString.lowercased()
        if path == "/api/call/audio/start" { archive.audioGeneration = id }
        if path == "/api/call/audio/stop" {
            guard let generation = archive.audioGeneration else { throw unknown("没有可确认的原模块音频代次，未提交关闭请求") }
            fields["audio_generation"] = generation
        }
        fields["client_request_id"] = id
        let body = try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
        archive.pending[path] = Intent(id: id, original: original, request: body)
        try save(archive)
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        return Token(path: path, id: id)
    }
    static func completeResponse(path: String, data: Data) throws {
        let clean = "/" + path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard routes.contains(clean),
              let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let receipt = object["mutation_receipt"] as? [String: Any],
              let id = receipt["client_request_id"] as? String else { return }
        try complete(Token(path: clean, id: id), data: data)
    }
    static func complete(_ token: Token?, data: Data) throws {
        guard let token else { return }
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let receipt = object["mutation_receipt"] as? [String: Any],
              receipt["client_request_id"] as? String == token.id else { return }
        if receipt["outcome"] as? String != "completed" { throw unknown("Rust 保留了原操作的未知结果，原请求不会重新提交") }
        lock.lock(); defer { lock.unlock() }
        var archive = try load()
        guard archive.pending[token.path]?.id == token.id else { return }
        let original = defaults.object(forKey: key)
        if token.path == "/api/call/audio/stop",
           let stop = archive.pending[token.path],
           let fields = (try? JSONSerialization.jsonObject(with: stop.request)) as? [String: Any],
           let generation = fields["audio_generation"] as? String,
           archive.pending["/api/call/audio/start"]?.id == generation {
            // A confirmed stop of this exact generation settles the preceding
            // start fence, allowing a later explicit route to get a new ID.
            archive.pending.removeValue(forKey: "/api/call/audio/start")
        }
        archive.pending.removeValue(forKey: token.path)
        do { try save(archive) } catch {
            // Retain the original identity when removing the fence is uncertain.
            if let original { defaults.set(original, forKey: key); _ = defaults.synchronize() }
            throw error
        }
    }
}
