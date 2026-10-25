import Foundation

// External services are intentionally absent: every request is suspended until
// a fixture supplies JSON or an error, including replies after cancellation.
struct FixtureUSIMInfo: Decodable { let iccid: String }
struct ReaderInfoResponse: Decodable {
    let usimInfo: FixtureUSIMInfo?
    enum CodingKeys: String, CodingKey { case usimInfo = "usim_info" }
}
func iccidFlagEmoji(_ value: String) -> String? { nil }

@MainActor
final class DashboardStore {
    static var shared: DashboardStore?
    var readerName: String?
    var readerConnected = false
    var readerHasESIM = false
    var readerUSIMInfo: FixtureUSIMInfo?
    var publishedOperators: [String?] = []
    var profileSynchronizations = 0
    func updateReaderProfileOperatorName(_ name: String?) { publishedOperators.append(name) }
    func synchronizeReaderAfterProfileSwitch(_ info: ReaderInfoResponse? = nil) {
        profileSynchronizations += 1
        readerUSIMInfo = info?.usimInfo
    }
}

enum FixtureError: Error { case failed, outcomeUnknown }
struct FixtureRefreshError: LocalizedError, ESIMRefreshFailure {
    let refreshOutcome: ESIMRefreshOutcome?
    var errorDescription: String? { refreshOutcome?.message }
}
// Classification itself is covered against the production APIClient by
// test-api-request-policy.sh; identity tests inject both outcome categories.
func operationOutcomeIsUnknown(_ error: any Error) -> Bool {
    if case FixtureError.outcomeUnknown = error { return true }
    return false
}

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

    func request(method: String, path: String, body: Data? = nil) async throws -> Data {
        if path == "api/esim/notes" { return Data(#"{"notes":{}}"#.utf8) }
        return try await withCheckedThrowingContinuation { continuation in
            requests.append(Request(method: method, path: path, body: body, continuation: continuation))
        }
    }
    func complete(_ index: Int, json: String) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(returning: Data(json.utf8))
    }
    func fail(_ index: Int, error: any Error = FixtureError.failed) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: error)
    }
    var pendingCount: Int { requests.count - completed.count }
}

@MainActor
struct APIClient {
    init(timeoutInterval: TimeInterval = 15) {}
    func get<T: Decodable>(_ path: String) async throws -> T {
        try await decode(method: "GET", path: path)
    }
    func send<T: Decodable, B: Encodable>(_ path: String, method: String = "POST", body: B) async throws -> T {
        try await decode(method: method, path: path, body: JSONEncoder().encode(body))
    }
    private func decode<T: Decodable>(method: String, path: String, body: Data? = nil) async throws -> T {
        let data = try await ControlledAPI.shared.request(method: method, path: path, body: body)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            let fractional = ISO8601DateFormatter()
            fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            if let date = fractional.date(from: text) { return date }
            let plain = ISO8601DateFormatter()
            plain.formatOptions = [.withInternetDateTime]
            guard let date = plain.date(from: text) else {
                throw DecodingError.dataCorruptedError(in: try decoder.singleValueContainer(), debugDescription: "invalid date")
            }
            return date
        }
        return try decoder.decode(T.self, from: data)
    }
}
