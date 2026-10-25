import Combine
import Foundation

@MainActor final class BackendProcess: ObservableObject {
    enum State: Equatable { case running, stopped }
    @Published var state: State = .running
}
struct DeviceStatus {}
struct CallStatus {}

@MainActor final class ControlledAPI {
    struct Request {
        let path: String
        let body: Data?
        let continuation: CheckedContinuation<Data, Error>
    }
    static let shared = ControlledAPI()
    var requests: [Request] = []
    var completed = Set<Int>()
    var usbnetTesting = false
    func request(_ path: String, body: Data?) async throws -> Data {
        guard path.hasPrefix("api/network/recovery")
                || path == "api/network/reboot-module"
                || (usbnetTesting && (path == "api/network" || path == "api/network/usbnet")) else {
            throw CocoaError(.fileReadUnknown)
        }
        return try await withCheckedThrowingContinuation { requests.append(Request(path: path, body: body, continuation: $0)) }
    }
    func complete(_ index: Int, _ json: String) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(returning: Data(json.utf8))
    }
    func fail(_ index: Int) {
        precondition(completed.insert(index).inserted)
        requests[index].continuation.resume(throwing: URLError(.networkConnectionLost))
    }
}

final class MemoryNetworkRecoveryCheckpoint: NetworkRecoveryCheckpointing {
    var pending: NetworkRecoveryPendingIntent?
    func load() throws -> NetworkRecoveryPendingIntent? { pending }
    func save(_ pending: NetworkRecoveryPendingIntent) throws { self.pending = pending }
    func clear(matching clientRequestID: String) throws {
        if pending?.clientRequestID == clientRequestID { pending = nil }
    }
}
struct APIClient {
    init(timeoutInterval: TimeInterval = 15) {}
    func get<T: Decodable>(_ path: String) async throws -> T {
        let bytes = try await ControlledAPI.shared.request(path, body: nil)
        return try JSONDecoder().decode(T.self, from: bytes)
    }
    func send<T: Decodable, B: Encodable>(_ path: String, body: B) async throws -> T {
        let bytes = try await ControlledAPI.shared.request(path, body: JSONEncoder().encode(body))
        return try JSONDecoder().decode(T.self, from: bytes)
    }
    func send<T: Decodable>(_ path: String) async throws -> T {
        let bytes = try await ControlledAPI.shared.request(path, body: nil)
        return try JSONDecoder().decode(T.self, from: bytes)
    }
}

// Real production checkpoint logic with faults at the UserDefaults boundary.
final class FaultingNetworkDefaults: UserDefaults {
    var failSynchronization = false
    var discardWrites = false
    var alterWriteBytes = false
    var discardRemoval = false
    override func synchronize() -> Bool {
        failSynchronization ? false : super.synchronize()
    }
    override func set(_ value: Any?, forKey key: String) {
        if discardWrites { return }
        if alterWriteBytes, let data = value as? Data {
            // Still decodes to the exact same DTO: only byte-for-byte readback
            // catches this mismatch at the admission boundary.
            super.set(data + Data(" ".utf8), forKey: key)
        } else {
            super.set(value, forKey: key)
        }
    }
    override func removeObject(forKey key: String) {
        if !discardRemoval { super.removeObject(forKey: key) }
    }
}
