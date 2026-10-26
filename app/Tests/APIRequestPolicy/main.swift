import Combine
import Foundation

@MainActor final class BackendProcess: ObservableObject {
    enum State { case running, stopped }
    @Published var state: State = .running
}

enum LocalBackendAuthentication {
    static let appToken = "isolated-test-token"
    static let headerName = "X-Test-Token"
}
final class UnixSocketURLProtocol: URLProtocol, @unchecked Sendable {
    private static let lock = NSLock()
    private static var recorded: [URLRequest] = []
    private static var failure: URLError.Code?
    private static var httpStatus = 200
    private static var responseBody = "{}"
    private static var responseHeaders: [String: String] = [:]
    static func configure(error: URLError.Code? = nil, status: Int = 200, body: String = "{}", headers: [String: String] = [:]) {
        lock.lock()
        defer { lock.unlock() }
        recorded = []
        failure = error
        httpStatus = status
        responseBody = body
        responseHeaders = headers
    }
    static var requests: [URLRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        var observed=request
        // URLSession may hand custom protocols an InputStream instead of Data.
        // Snapshot it here; this mock never forwards the request to a network.
        if observed.httpBody == nil, let stream=observed.httpBodyStream {
            stream.open();var bytes=Data();var buffer=[UInt8](repeating:0,count:4096)
            while stream.hasBytesAvailable {let count=stream.read(&buffer,maxLength:buffer.count);if count<=0 {break};bytes.append(buffer,count:count)}
            stream.close();observed.httpBody=bytes
        }
        Self.lock.lock()
        Self.recorded.append(observed)
        let isCallStatus = request.url?.path == "/api/call/status"
        let failure = isCallStatus ? nil : Self.failure
        let status = isCallStatus ? 200 : Self.httpStatus
        let body = isCallStatus ? #"{"state":"idle"}"# : Self.responseBody
        let headers = Self.responseHeaders
        Self.lock.unlock()
        if let failure {
            client?.urlProtocol(self, didFailWithError: URLError(failure))
        } else {
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
    }
    override func stopLoading() {}
}

struct CheckFailure: Error, CustomStringConvertible { let description: String }
func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CheckFailure(description: message) }
}
struct EmptyResponse: Decodable {}
private struct CallAcceptedResult: Decodable { let accepted: Bool }
private struct OptionalESIMMutationResult: Decodable { let message: String?; let phase: String? }
private struct AcceptedESIMOperation: Decodable {
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
private struct RadioATInput: Encodable { let command:String }
private struct RequiredRadioResult:Decodable {let started:Bool}

@main enum APIRequestPolicyTests {

    @MainActor private static func waitUntil(_ predicate: () -> Bool) async throws {
        for _ in 0..<500 {
            if predicate() { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw CheckFailure(description: "call recovery fixture did not settle")
    }

    @MainActor private static func callRecoveryUsesProductionErrorClassification() async throws {
        let suite = "switch-call-api-recovery-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }

        // A genuinely first, unconnected request is still an ordinary failure.
        UnixSocketURLProtocol.configure(error: .cannotConnectToHost)
        let initial = CallRuntimeStore(backend: BackendProcess(), defaults: defaults)
        initial.dial("15550001001")
        try await waitUntil { initial.error != nil }
        try expect(!initial.commandInFlight, "first connect failure retained an unknown fence")
        try expect(defaults.object(forKey: "switch.call.pendingMutation.v1") == nil,
                   "first unsubmitted call retained its checkpoint")

        UnixSocketURLProtocol.configure(error: .timedOut)
        let original = CallRuntimeStore(backend: BackendProcess(), defaults: defaults)
        original.dial("15550001002")
        try await waitUntil { original.error != nil }
        let originalPayload = UnixSocketURLProtocol.requests.first { $0.url?.path == "/api/call/dial" }!.httpBody!
        let body = try JSONSerialization.jsonObject(with: originalPayload) as! [String: Any]
        let requestID = body["client_request_id"] as! String
        let checkpoint = defaults.data(forKey: "switch.call.pendingMutation.v1")
        try expect(original.commandInFlight && checkpoint != nil, "lost initial reply cleared the call fence")

        // Exercise the actual APIClient classifications, not the old adapter's
        // replacement unknown predicate. Each GUI reopen reuses exact bytes.
        for failure in ["connect", "receipt-unavailable", "ordinary-http", "wrong-id", "terminal-unknown"] {
            if failure == "connect" {
                UnixSocketURLProtocol.configure(error: .cannotConnectToHost)
            } else {
                let code = failure == "receipt-unavailable" ? "call_mutation_receipt_unavailable"
                    : failure == "terminal-unknown" ? "call_mutation_receipt_outcome_unknown" : "call_owner_busy"
                let headers = ["wrong-id", "terminal-unknown"].contains(failure)
                    ? ["X-Switch-Call-Terminal": "true",
                       "X-Switch-Call-Request-ID": failure == "wrong-id" ? "another-request" : requestID] : [:]
                UnixSocketURLProtocol.configure(status: 503,
                    body: "{\"error\":\"fixture rejection\",\"code\":\"\(code)\"}", headers: headers)
            }
            let reopened = CallRuntimeStore(backend: BackendProcess(), defaults: defaults)
            reopened.poll()
            try await waitUntil { reopened.error != nil }
            try expect(reopened.commandInFlight, "recovery failure cleared original fence: \(failure)")
            try expect(defaults.data(forKey: "switch.call.pendingMutation.v1") == checkpoint,
                       "recovery failure changed durable bytes: \(failure)")
            let submits = UnixSocketURLProtocol.requests.filter { $0.url?.path == "/api/call/dial" }
            try expect(submits.count == 1 && submits[0].httpBody == originalPayload,
                       "recovery re-encoded or replaced the original payload")
            let count = UnixSocketURLProtocol.requests.count
            reopened.dial("15550009999")
            reopened.answer()
            reopened.hangup()
            try expect(UnixSocketURLProtocol.requests.count == count,
                       "recovery failure permitted a new call command")
        }

        UnixSocketURLProtocol.configure(status: 409,
            body: #"{"error":"terminal fixture rejection","code":"call_owner_busy"}"#,
            headers: ["X-Switch-Call-Terminal": "true", "X-Switch-Call-Request-ID": requestID])
        let terminal = CallRuntimeStore(backend: BackendProcess(), defaults: defaults)
        terminal.poll()
        try await waitUntil { terminal.error != nil }
        try expect(!terminal.commandInFlight && defaults.object(forKey: "switch.call.pendingMutation.v1") == nil,
                   "matching durable negative receipt did not settle original fence")
        print("PASS: production API + CallRuntime preserve recovery failures and accept only matching durable negative receipts")
    }
    @MainActor static func main() async throws {
        try await callRecoveryUsesProductionErrorClassification()
        let radioSuite="switch-radio-api-policy-"+UUID().uuidString
        PublicRadioMutationCheckpoint.defaults=UserDefaults(suiteName:radioSuite)!
        defer {PublicRadioMutationCheckpoint.defaults.removePersistentDomain(forName:radioSuite)}

        for path in ["api/module/flight-mode", "/api/module/flight-mode?fixture=1"] {
            try expect(APIClient.readTimeout(path:path,defaultTimeout:12)==100,"RF read deadline missing")
        }
        try expect(APIClient.readTimeout(path:"api/module/flight-mode-extra",defaultTimeout:12)==12,"RF read prefix widened")
        print("PASS: exact read-only budgets remain isolated from mutation semantics")

        for (path, method, configured) in [("api/sms/send", "POST", 30.0),
                                           ("api/network/reboot-module", "POST", 47.0),
                                           ("api/esim/profile", "PATCH", 61.0),
                                           ("api/reader/esim/profile", "DELETE", 73.0),
                                           ("api/future/mutation", "POST", 89.0)] {
            UnixSocketURLProtocol.configure()
            let _: EmptyResponse = try await APIClient(timeoutInterval: configured).send(path, method: method)
            try expect(UnixSocketURLProtocol.requests.count == 1, "successful write repeated")
            try expect(UnixSocketURLProtocol.requests.first?.timeoutInterval == configured,
                       "mutation waiter used a route-specific business deadline")
            try expect(UnixSocketURLProtocol.requests.first?.value(forHTTPHeaderField: "X-Test-Token") == "isolated-test-token",
                       "mutation request dropped authentication")
        }
        print("PASS: every mutation uses only the caller transport budget; no business-route timeout table")

        for code in [URLError.Code.timedOut, .networkConnectionLost, .badServerResponse, .cancelled] {
            UnixSocketURLProtocol.configure(error: code)
            do {
                try await APIClient().send("api/future/mutation")
                throw CheckFailure(description: "transport failure succeeded")
            } catch let error as APIOperationOutcomeUnknownError {
                try expect(operationOutcomeIsUnknown(error) && smsSendErrorMessage(error).contains("不要重复提交"),
                           "ambiguous write did not warn against duplicate submission")
                try expect(!smsSendErrorMessage(error).hasPrefix("发送失败"), "unknown result labelled definite failure")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1, "failed write automatically retried")
        }
        print("PASS: ambiguous transport failures are pending confirmation; zero automatic retries")

        for path in ["api/future/mutation", "api/call-mode/restore"] {
            UnixSocketURLProtocol.configure(body: "unparseable 2xx response")
            do {
                let _: EmptyResponse = try await APIClient().send(path)
                throw CheckFailure(description: "malformed write result succeeded")
            } catch let error as APIOperationOutcomeUnknownError {
                try expect(error.localizedDescription.contains("无法解析操作结果"), "2xx decode error encouraged resubmission")
                try expect(mutationFailureRequiresStatusReconciliation(error), "unknown receipt did not retain mutation fence")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1, "malformed 2xx write automatically retried")
        }
        UnixSocketURLProtocol.configure(body: "unparseable 2xx upload response")
        do {
            let _: EmptyResponse = try await APIClient().upload("api/call-mode/backups/import", data: Data("{}".utf8))
            throw CheckFailure(description: "malformed upload result succeeded")
        } catch is APIOperationOutcomeUnknownError {}
        try expect(UnixSocketURLProtocol.requests.count == 1, "malformed 2xx upload automatically retried")
        print("PASS: every typed write/upload with malformed 2xx remains pending confirmation")

        for action in ["dial", "answer", "hangup", "dtmf"] {
            UnixSocketURLProtocol.configure(status: 202, body: #"{"phase":"unknown","outcome":"unknown","selected_owner":"module","generation":0,"dispatch_count":0}"#)
            do {
                let _: CallAcceptedResult = try await APIClient().upload(
                    "api/call/\(action)", data: Data(#"{"client_request_id":"original-call-request"}"#.utf8))
                throw CheckFailure(description: "unbound module receipt became a terminal call result")
            } catch let error as APIOperationOutcomeUnknownError {
                try expect(mutationFailureRequiresStatusReconciliation(error),
                           "unbound module receipt released the call mutation fence")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1,
                       "unbound module receipt caused an automatic POST retry")
        }
        print("PASS: module admitted 202/unknown keeps the call fence for dial/answer/hangup/dtmf")

        UnixSocketURLProtocol.configure(error: .cannotConnectToHost)
        do {
            try await APIClient().send("api/sms/send")
            throw CheckFailure(description: "disconnected service succeeded")
        } catch let error as URLError {
            try expect(!operationOutcomeIsUnknown(error), "failed connect falsely reported submitted")
        }
        UnixSocketURLProtocol.configure(error: .timedOut)
        do {
            let _: EmptyResponse = try await APIClient().get("api/sms/status")
            throw CheckFailure(description: "GET timeout succeeded")
        } catch let error as URLError {
            try expect(error.code == .timedOut, "GET timeout semantics changed")
        }
        UnixSocketURLProtocol.configure(status: 504, body: "{\"error\":\"短信结果待确认，已确认 1/2 段，请勿重发\",\"code\":\"sms_send_outcome_unknown\",\"recoverable\":false}")
        do {
            try await APIClient().send("api/sms/send")
            throw CheckFailure(description: "unknown remote outcome succeeded")
        } catch let error as APIError {
            try expect(operationOutcomeIsUnknown(error) && smsSendErrorMessage(error).contains("1/2"),
                       "backend partial-segment evidence lost")
            try expect(!error.isRecoverable && !smsSendErrorMessage(error).hasPrefix("发送失败"),
                       "backend uncertain outcome was retryable or marked failed")
        }
        print("PASS: ordinary failures preserved; server uncertainty and confirmed segments retained")

        for (code, status) in [("esim_mutation_receipt_unavailable", 503),
                               ("esim_mutation_dispatch_already_claimed", 409),
                               ("esim_mutation_request_id_conflict", 409)] {
            UnixSocketURLProtocol.configure(status: status, body: "{\"error\":\"请按原请求编号查询结果\",\"code\":\"\(code)\"}")
            do {
                try await APIClient().send("api/esim/profile")
                throw CheckFailure(description: "uncertain eSIM mutation succeeded: \(code)")
            } catch let error as APIError {
                try expect(error.statusCode == status && error.code == code, "eSIM mutation error payload was lost: \(code)")
                try expect(operationOutcomeIsUnknown(error) && mutationFailureRequiresStatusReconciliation(error),
                           "uncertain eSIM mutation released its write checkpoint: \(code)")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1, "uncertain eSIM mutation was retried: \(code)")
        }
        for (code, status) in [("esim_mutation_intent_unavailable", 503),
                               ("esim_mutation_owner_unavailable", 409)] {
            UnixSocketURLProtocol.configure(status: status, body: "{\"error\":\"尚未操作卡片\",\"code\":\"\(code)\"}")
            do {
                try await APIClient().send("api/esim/profile")
                throw CheckFailure(description: "unsubmitted eSIM mutation succeeded: \(code)")
            } catch let error as APIError {
                try expect(error.statusCode == status && error.code == code, "unsubmitted eSIM error payload was lost: \(code)")
                try expect(!operationOutcomeIsUnknown(error) && !mutationFailureRequiresStatusReconciliation(error),
                           "unsubmitted eSIM mutation retained its write checkpoint: \(code)")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1, "unsubmitted eSIM mutation was retried: \(code)")
        }
        print("PASS: eSIM receipt and claim uncertainty retain checkpoints; pre-dispatch failures release them")

        let esimRoutes: [(String, String, String, String)] = [
            ("api/esim/switch", "POST", "module", "switch"),
            ("api/esim/profile", "DELETE", "module", "delete"),
            ("api/esim/profile", "PATCH", "module", "rename"),
            ("api/esim/download", "POST", "module", "download"),
            ("api/reader/esim/switch", "POST", "reader", "switch"),
            ("api/reader/esim/profile", "DELETE", "reader", "delete"),
            ("api/reader/esim/profile", "PATCH", "reader", "rename"),
            ("api/reader/esim/download", "POST", "reader", "download")
        ]
        for (path, method, scope, action) in esimRoutes {
            for phase in ["prepared", "submitted", "unknown"] {
                let receipt: [String: Any] = [
                    "client_request_id": "original-esim-request", "action": action,
                    "scope": scope, "phase": phase, "operation_id": "original-operation",
                    "dispatch_count": phase == "prepared" ? 0 : 1
                ]
                let body = String(data: try JSONSerialization.data(withJSONObject: receipt), encoding: .utf8)!
                UnixSocketURLProtocol.configure(status: 202, body: body)
                do {
                    let _: OptionalESIMMutationResult = try await APIClient().send(path, method: method)
                    throw CheckFailure(description: "nonterminal eSIM intent decoded as success: \(path) \(phase)")
                } catch let error as APIOperationOutcomeUnknownError {
                    try expect(operationOutcomeIsUnknown(error) && mutationFailureRequiresStatusReconciliation(error),
                               "nonterminal eSIM intent released status reconciliation: \(path) \(phase)")
                }
                try expect(UnixSocketURLProtocol.requests.count == 1,
                           "nonterminal eSIM intent caused another mutation: \(path) \(phase)")
            }
        }
        for (path, method, _, _) in esimRoutes {
            UnixSocketURLProtocol.configure(status: 202, body: "{}")
            do {
                let _: OptionalESIMMutationResult = try await APIClient().send(path, method: method)
                throw CheckFailure(description: "empty 202 eSIM result decoded as success: \(path)")
            } catch let error as APIOperationOutcomeUnknownError {
                try expect(mutationFailureRequiresStatusReconciliation(error),
                           "empty 202 eSIM result released status reconciliation: \(path)")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1,
                       "empty 202 eSIM result caused another mutation: \(path)")
        }
        for path in ["api/esim/download", "api/reader/esim/download"] {
            UnixSocketURLProtocol.configure(status: 202, body: #"{"id":"download-operation","kind":"download_profile","state":"running","step":"preflight","progress":5,"started_at":"2026-10-22T00:00:00Z","updated_at":"2026-10-22T00:00:01Z"}"#)
            let operation: AcceptedESIMOperation = try await APIClient().send(path)
            try expect(operation.id == "download-operation" && operation.kind == "download_profile"
                       && operation.state == "running" && operation.progress == 5
                       && operation.startedAt < operation.updatedAt,
                       "normal 202 eSIM download operation was rejected: \(path)")
            try expect(UnixSocketURLProtocol.requests.count == 1, "normal 202 eSIM download was repeated: \(path)")
        }
        print("PASS: eSIM intent and empty 202 responses remain unknown on module/reader routes; complete 202 download operations decode")

        UnixSocketURLProtocol.configure(status: 502, body: "{\"error\":\"发送通道已接受请求，但本机记录未确认。再次发送可能重复，请先核对。\",\"code\":\"sms_send_archive_outcome_unknown\"}")
        do {
            try await APIClient().send("api/sms/send")
            throw CheckFailure(description: "submitted SMS with failed archive was treated as success")
        } catch let error as APIError {
            try expect(error.statusCode == 502 && error.code == "sms_send_archive_outcome_unknown",
                       "archive uncertainty status/code was lost")
            try expect(operationOutcomeIsUnknown(error) && !error.isRecoverable,
                       "submitted SMS with failed archive encouraged automatic retry")
            let message = smsSendErrorMessage(error)
            try expect(!message.contains("结果待确认") && message.contains("发送通道已接受请求") && message.contains("再次发送可能重复"),
                       "archive uncertainty lost submission evidence or duplicate warning")
            try expect(!message.hasPrefix("发送失败"),
                       "archive failure incorrectly labelled the SMS submission as failed")
        }
        try expect(UnixSocketURLProtocol.requests.count == 1,
                   "archive uncertainty caused another SMS POST")
        try expect(UnixSocketURLProtocol.requests.first?.httpMethod == "POST",
                   "archive uncertainty fixture did not exercise an actual write")
        print("PASS: submitted SMS with archive failure remains unknown; one POST and no send-failure prefix")
        let priorRequests = UnixSocketURLProtocol.requests.count
        for error: any Error in [CancellationError(), URLError(.cancelled)] {
            let message = smsSendErrorMessage(error)
            try expect(message.contains("查询原发送记录") && !message.hasPrefix("发送失败"),
                       "cancelled confirmation was presented as definite non-submission")
            try expect(mutationFailureRequiresStatusReconciliation(error),
                       "cancelled mutation waiter released its status-reconciliation fence")
        }
        try expect(UnixSocketURLProtocol.requests.count == priorRequests, "cancelled waiter presentation triggered I/O")
        print("PASS: canceled submission wait asks for original-record lookup without claiming failure")
        for path in ["api/sms/delete", "api/sms/delete-sender", "api/sms/clear"] {
            UnixSocketURLProtocol.configure()
            let _: EmptyResponse = try await APIClient().send(path)
            try expect(UnixSocketURLProtocol.requests.count == 1 && UnixSocketURLProtocol.requests[0].timeoutInterval == 30,
                       "storage mutation used route-specific deadline or repeated")
            UnixSocketURLProtocol.configure(error: .timedOut)
            do { try await APIClient().send(path); throw CheckFailure(description: "storage timeout succeeded") }
            catch is APIOperationOutcomeUnknownError {}
            try expect(UnixSocketURLProtocol.requests.count == 1, "storage mutation was retried")
        }
        for (path, expected) in [("api/module/flight-mode", 100.0), ("api/sms/storage", 180.0), ("api/sms/storage-extra", 30.0), ("api/status", 30.0)] {
            UnixSocketURLProtocol.configure()
            let _: EmptyResponse = try await APIClient().get(path)
            try expect(UnixSocketURLProtocol.requests[0].timeoutInterval == expected,
                       "exact storage/RF GET budget missing or unrelated GET changed")
            try expect(UnixSocketURLProtocol.requests.first?.value(forHTTPHeaderField: "X-Test-Token") == "isolated-test-token", "read session dropped authentication")
        }
        UnixSocketURLProtocol.configure(error: .timedOut)
        do { let _: EmptyResponse = try await APIClient().get("api/sms/storage"); throw CheckFailure(description: "storage GET succeeded") }
        catch let error as URLError { try expect(error.code == .timedOut, "storage GET was classified as a mutation") }
        try expect(UnixSocketURLProtocol.requests.count == 1, "storage GET automatically repeated")
        print("PASS: storage GET keeps its read budget; mutation waiter stays uniform and never replays")
        print("API request policy: in-memory phase passed (no sockets/credentials/hardware)")
        for path in ["api/reader/esim?refresh=true", "api/esim?refresh=true"] {
            UnixSocketURLProtocol.configure(status: 503, body: #"{"error":"synthetic","code":"esim_refresh_cleanup_pending","refresh":{"operation_id":"intent-1","scope":"reader","phase":"pending","outcome":"cleanup_pending","attempts":1,"retry_performed":false,"message":"清理未确认，本次未重试"}}"#)
            do {
                let _: EmptyResponse = try await APIClient().get(path)
                throw CheckFailure(description: "pending refresh succeeded")
            } catch let error as APIError {
                try expect(error.refreshOutcome?.phase == "pending" && error.refreshOutcome?.outcome == "cleanup_pending",
                           "structured refresh failure was discarded")
                try expect(!error.isRecoverable && error.errorDescription == "清理未确认，本次未重试", "Swift inferred a retry policy")
            }
            try expect(UnixSocketURLProtocol.requests.count == 1, "refresh intent automatically repeated")
        }
        print("PASS: both eSIM refresh prefixes retain structured phase/outcome and send once")
        UnixSocketURLProtocol.configure(error:.timedOut)
        do {try await APIClient().send("api/at",body:RadioATInput(command:"AT+UNKNOWNWRITE=1"));throw CheckFailure(description:"radio timeout succeeded")}catch is APIOperationOutcomeUnknownError{}
        let originalRadioBody=UnixSocketURLProtocol.requests[0].httpBody!
        let radioFields=try JSONSerialization.jsonObject(with:originalRadioBody) as! [String:Any]
        let radioID=radioFields["client_request_id"] as! String
        UnixSocketURLProtocol.configure(error:.networkConnectionLost)
        do {try await APIClient().send("api/at",body:RadioATInput(command:"AT+UNKNOWNWRITE=1"));throw CheckFailure(description:"radio reconnect succeeded")}catch is APIOperationOutcomeUnknownError{}
        try expect(UnixSocketURLProtocol.requests.count==1 && UnixSocketURLProtocol.requests[0].httpBody==originalRadioBody,"new APIClient regenerated unknown radio intent")
        UnixSocketURLProtocol.configure()
        do {try await APIClient().send("api/at",body:RadioATInput(command:"AT+UNKNOWNWRITE=2"));throw CheckFailure(description:"radio payload drift uploaded")}catch is APIOperationOutcomeUnknownError{}
        try expect(UnixSocketURLProtocol.requests.isEmpty,"unknown radio payload drift reached transport")
        let completedRadioReceipt=String(data:try JSONSerialization.data(withJSONObject:["mutation_receipt":["client_request_id":radioID,"outcome":"completed"]]),encoding:.utf8)!
        UnixSocketURLProtocol.configure(body:completedRadioReceipt)
        do {let _:RequiredRadioResult=try await APIClient().send("api/at",body:RadioATInput(command:"AT+UNKNOWNWRITE=1"));throw CheckFailure(description:"radio malformed typed result succeeded")}catch is APIOperationOutcomeUnknownError{}
        UnixSocketURLProtocol.configure(body:completedRadioReceipt)
        let _:EmptyResponse=try await APIClient().send("api/at",body:RadioATInput(command:"AT+UNKNOWNWRITE=1"))
        try expect(UnixSocketURLProtocol.requests[0].httpBody==originalRadioBody,"2xx decode failure cleared caller identity")
        UnixSocketURLProtocol.configure(error:.timedOut)
        do {try await APIClient().send("api/at",body:RadioATInput(command:"AT+UNKNOWNWRITE=2"));throw CheckFailure(description:"new radio timeout succeeded")}catch is APIOperationOutcomeUnknownError{}
        let laterFields=try JSONSerialization.jsonObject(with:UnixSocketURLProtocol.requests[0].httpBody!) as! [String:Any]
        try expect(laterFields["client_request_id"] as? String != radioID,"confirmed new radio action reused old identity")
        print("PASS: actual APIClient persists radio ID/body across lost responses and replacement, blocks drift before upload, and retains decode-failed receipts")
        let retainedRadioJournal = PublicRadioMutationCheckpoint.defaults.data(forKey: "publicRadioMutationCheckpoint.v1")!
        for path in ["api/reader/vowifi/stop-control", "api/vowifi/stop-control"] {
            let scope = path.contains("reader") ? "reader" : "module"
            let query = "scope=\(scope)&daemon_epoch=epoch-1&owner_id=owner-1&owner_generation=7"
            UnixSocketURLProtocol.configure(body: "{}")
            let _: EmptyResponse = try await APIClient().upload("\(path)?\(query)", data: Data())
            try expect(UnixSocketURLProtocol.requests.count == 1 && (UnixSocketURLProtocol.requests[0].httpBody?.isEmpty ?? true)
                           && UnixSocketURLProtocol.requests[0].httpBodyStream == nil,
                       "temporary stop was replayed or acquired a nonempty/unknown body")
            try expect(UnixSocketURLProtocol.requests[0].url?.query == query, "temporary target query was rewritten")
            try expect(PublicRadioMutationCheckpoint.defaults.data(forKey: "publicRadioMutationCheckpoint.v1") == retainedRadioJournal,
                       "temporary stop changed unrelated permanent radio checkpoint")
            PublicRadioMutationCheckpoint.defaults.set(Data("corrupt".utf8), forKey: "publicRadioMutationCheckpoint.v1")
            var request = URLRequest(url: URL(string: "http://fixture/\(path)?\(query)")!)
            request.httpMethod = "POST"; request.httpBody = Data()
            let token = try PublicRadioMutationCheckpoint.prepare(&request)
            try expect(token == nil,
                       "unrelated corrupt radio journal blocked temporary stop")
            try expect(request.httpBody == Data(), "non-allowlisted empty stop payload was rewritten")
            PublicRadioMutationCheckpoint.defaults.set(retainedRadioJournal, forKey: "publicRadioMutationCheckpoint.v1")
        }
        print("PASS: production APIClient stop-control uploads bypass permanent radio journal and preserve exact empty body/query")


    }
}
