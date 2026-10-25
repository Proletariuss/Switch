import Foundation
import Combine

/// Hardware-free API fixture. Every response is explicitly released by a test;
/// it never opens a socket or starts the installed application.
enum FixtureError: Error { case failed, outcomeUnknown }

struct APIError: LocalizedError {
    let code: String?
    var errorDescription: String? { code }
}

func mutationFailureRequiresStatusReconciliation(_ error: any Error) -> Bool {
    (error as? FixtureError) == .outcomeUnknown
}

func readerLifecycleFailureWasDefinitelyNotSubmitted(_ error: any Error) -> Bool { false }

@MainActor
final class ControlledAPI {
    struct Request {
        let method: String
        let path: String
        let body: Data?
        let continuation: CheckedContinuation<Data, Error>
    }

    static let shared = ControlledAPI()
    private(set) var requests: [Request] = []
    private var completed: Set<Int> = []
    var readerListJSON = #"{"readers":["Fixture Reader"],"selected":"Fixture Reader"}"#
    var suspendReaderLists = false
    var connectBodies: [Data] = []
    func recordConnect(_ bytes: Data) { connectBodies.append(bytes) }

    func request(method: String, path: String, body: Data? = nil) async throws -> Data {
        // These independent reads are deliberately not part of the failure
        // fixture. They keep the aggregate fallback focused on VoWiFi.
        if path == "api/reader/list", !suspendReaderLists {
            return Data(readerListJSON.utf8)
        }
        if path == "api/network/traffic" { return Data("{}".utf8) }
        return try await withCheckedThrowingContinuation { continuation in
            requests.append(Request(method: method, path: path, body: body, continuation: continuation))
        }
    }

    func complete(_ index: Int, json: String) {
        precondition(completed.insert(index).inserted)
        var response = json
        if response.contains("\"operation\"") && !response.contains("\"client_request_id\"") {
            let request = requests[index]
            let bodyID = request.body.flatMap { data -> String? in
                (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["client_request_id"] as? String
            }
            let queryID = URLComponents(string: "fixture://host/\(request.path)")?
                .queryItems?.first(where: { $0.name == "client_request_id" })?.value
            if let requestID = bodyID ?? queryID,
               let encoded = try? JSONEncoder().encode(requestID),
               let literal = String(data: encoded, encoding: .utf8) {
                if let range = response.range(of: "\"operation_id\"") {
                    response.replaceSubrange(
                        range,
                        with: "\"client_request_id\":\(literal),\"operation_id\"")
                }
            }
        }
        requests[index].continuation.resume(returning: Data(response.utf8))
    }

    func fail(_ index: Int) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: FixtureError.failed)
    }

    func fail(_ index: Int, error: any Error) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: error)
    }

    func failUnknown(_ index: Int) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: FixtureError.outcomeUnknown)
    }

    var pendingCount: Int { requests.count - completed.count }
}

@MainActor
final class BackendProcess {
    enum State { case running }
    @Published var state: State = .running
}

struct EmptyResponse: Decodable {}
struct TrafficSnapshot: Decodable {}

func flagEmoji(countryCode: String) -> String { "" }
func iccidFlagEmoji(_ iccid: String) -> String? { nil }
func isModuleDisconnectedError(_ message: String) -> Bool { false }
let moduleDisconnectedFriendlyMessage = "fixture module disconnected"

struct APIClient {
    init(timeoutInterval: TimeInterval = 15) {}

    func get<T: Decodable>(_ path: String) async throws -> T {
        try await decode(method: "GET", path: path)
    }

    func upload<T: Decodable>(_ path: String, data: Data, contentType: String = "application/json",
                              as type: T.Type = T.self) async throws -> T {
        try await decode(method: "POST", path: path, body: data)
    }

    func send<T: Decodable, B: Encodable>(
        _ path: String,
        method: String = "POST",
        body: B,
        as: T.Type = T.self
    ) async throws -> T {
        let bytes = try JSONEncoder().encode(body)
        if path == "api/reader/connect" { await ControlledAPI.shared.recordConnect(bytes) }
        return try await decode(method: method, path: path, body: bytes)
    }

    func send<T: Decodable>(
        _ path: String,
        method: String = "POST",
        as: T.Type = T.self
    ) async throws -> T {
        try await decode(method: method, path: path)
    }

    private func decode<T: Decodable>(method: String, path: String, body: Data? = nil) async throws -> T {
        let data = try await ControlledAPI.shared.request(method: method, path: path, body: body)
        // Older store fixtures used `{}` as shorthand for a completed stop.
        // Preserve that shorthand as an explicit Rust cleanup receipt so the
        // production Store never needs a fail-open legacy branch.
        if T.self == VoWiFiAccepted.self,
           method == "POST",
           String(decoding: data, as: UTF8.self) == "{}" {
            let requestID = body.flatMap { data -> String? in
                guard let value = try? JSONSerialization.jsonObject(with: data),
                      let object = value as? [String: Any] else { return nil }
                return object["client_request_id"] as? String
            }
            let encodedID = requestID.map { "\"client_request_id\":\"\($0)\"," } ?? ""
            let receipt = """
            {"operation":{\(encodedID)"operation_id":"fixture-stop-1","generation":1,
             "requested_enable":false,"phase":"stopped","outcome":"stopped",
             "terminal":true,"succeeded":true,"observation_expired":false,
             "cleanup_confirmed":true,"poll_after_ms":0,"message":"fixture stop"}}
            """
            return try JSONDecoder().decode(T.self, from: Data(receipt.utf8))
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}

func fixtureCheckpointRoot() -> URL {
    URL(fileURLWithPath: ProcessInfo.processInfo.environment["SWITCH_DATA_DIR"]!)
        .appendingPathComponent("vowifi-gui-tests-\(UUID().uuidString)", isDirectory: true)
}
