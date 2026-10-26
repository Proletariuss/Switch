import Combine
import Foundation

@MainActor final class BackendProcess: ObservableObject {
    enum State: Equatable { case running, stopped }
    @Published var state: State = .running
}

@MainActor final class ControlledAPI {
    struct Request {
        let path: String
        let body: Data?
        let continuation: CheckedContinuation<Data, Error>
    }
    static let shared = ControlledAPI()
    var requests: [Request] = []
    private var completed = Set<Int>()
    func request(_ path: String, body: Data? = nil) async throws -> Data {
        try await withCheckedThrowingContinuation {
            requests.append(.init(path: path, body: body, continuation: $0))
        }
    }
    func complete(_ index: Int, json: String) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(returning: Data(json.utf8))
    }
    func fail(_ index: Int) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: NSError(domain: "fixture", code: 1))
    }
    func failUnknown(_ index: Int) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: APIOperationOutcomeUnknownError())
    }
    func finishPending() {
        for index in requests.indices where !completed.contains(index) { fail(index) }
    }
}

struct APIClient {
    init(timeoutInterval: TimeInterval = 30) {}
    func get<T: Decodable>(_ path: String) async throws -> T { try await response(path) }
    func send<T: Decodable, B: Encodable>(_ path: String, body: B) async throws -> T {
        try await response(path, body: JSONEncoder().encode(body))
    }
    func send<T: Decodable>(_ path: String) async throws -> T { try await response(path) }
    func send<T: Decodable, B: Encodable>(_ path: String, method: String, body: B) async throws -> T {
        try await response(path, body: JSONEncoder().encode(body))
    }
    func upload<T: Decodable>(
        _ path: String, data: Data, contentType: String = "application/json",
        as type: T.Type = T.self
    ) async throws -> T {
        let responseData = try await ControlledAPI.shared.request(path, body: data)
        do {
            return try JSONDecoder().decode(T.self, from: responseData)
        } catch {
            // Match production APIClient.upload's 2xx decode boundary: a
            // receipt without a terminal action result keeps the GUI fence.
            throw APIOperationOutcomeUnknownError()
        }
    }
    private func response<T: Decodable>(_ path: String, body: Data? = nil) async throws -> T {
        let data = try await ControlledAPI.shared.request(path, body: body)
        return try JSONDecoder().decode(T.self, from: data)
    }
}

// Production API/receipt integration is exercised separately by APIRequestPolicy.
struct APICallTerminalError: LocalizedError {
    let clientRequestID: String
    var errorDescription: String? { "fixture terminal call" }
}

struct APIOperationOutcomeUnknownError: LocalizedError {
    var errorDescription: String? { "操作结果待确认" }
}

func operationOutcomeIsUnknown(_ error: any Error) -> Bool {
    error is APIOperationOutcomeUnknownError
}

func mutationFailureRequiresStatusReconciliation(_ error: any Error) -> Bool {
    operationOutcomeIsUnknown(error) || error is CancellationError
}

struct DeviceStatus: Decodable { let usbnetMode: Int? }

@MainActor final class DashboardStore {
    let callRuntime: CallRuntimeStore
    init(_ runtime: CallRuntimeStore) { callRuntime = runtime }
    var callStatus: CallStatus { callRuntime.status }
    var callEndpoint: String { callRuntime.endpoint }
    var readerCallState: ReaderVoWiFiCallState? { callRuntime.readerState }
    var voiceError: String? { callRuntime.error }
    var callModeStatus: CallModeStatus? { nil }
}

struct MessageResponse: Decodable { let message: String }
