import Combine
import Foundation

enum LocalBackendAuthentication {
    static let appToken = "call-mode-fence-test"
    static let headerName = "X-Test-Token"
}

@MainActor
final class BackendProcess: ObservableObject {
    enum State: Equatable { case stopped, running }
    @Published var state: State = .running
}

final class UnixSocketURLProtocol: URLProtocol, @unchecked Sendable {
    enum Step {
        case failure(URLError.Code)
        case response(Int, String)
    }

    private static let lock = NSLock()
    private static var steps: [Step] = []
    private static var recorded: [URLRequest] = []

    static func configure(_ newSteps: [Step]) {
        lock.lock()
        steps = newSteps
        recorded = []
        lock.unlock()
    }

    static var requests: [URLRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lock.lock()
        Self.recorded.append(request)
        let step = Self.steps.isEmpty ? nil : Self.steps.removeFirst()
        Self.lock.unlock()
        switch step {
        case let .failure(code):
            client?.urlProtocol(self, didFailWithError: URLError(code))
        case let .response(status, body):
            let response = HTTPURLResponse(
                url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        case nil:
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
        }
    }

    override func stopLoading() {}
}

struct TestFailure: Error, CustomStringConvertible { let description: String }

func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw TestFailure(description: message) }
}

@MainActor
func waitUntil(_ predicate: () -> Bool) async throws {
    for _ in 0..<100 {
        if predicate() { return }
        try await Task.sleep(nanoseconds: 10_000_000)
    }
    throw TestFailure(description: "timed out waiting for store state")
}

let busyStatus = #"{"state":"enabling_usb","summary":"accepted","detail":null,"adb_enabled":false,"interfaces_ready":false,"adb_authorization_required":false,"uac_enabled":false,"voice_configured":false,"runtime_downloaded":false,"runtime_version":"","runtime_path":"","downloaded_bytes":0,"total_bytes":0,"source":null,"can_enable":false,"can_download":false,"can_retry":false,"requires_adb_authorization_confirmation":false,"requires_usb_confirmation":false,"requires_download_confirmation":false,"backup_path":null,"last_restore":null,"updated_at":"2026-10-22T00:00:00Z","sources":[],"upstream":"rust"}"#
let readyStatus = #"{"state":"ready","summary":"ready","detail":null,"adb_enabled":true,"interfaces_ready":true,"adb_authorization_required":false,"uac_enabled":true,"voice_configured":true,"runtime_downloaded":true,"runtime_version":"fixture","runtime_path":"fixture","downloaded_bytes":1,"total_bytes":1,"source":null,"can_enable":false,"can_download":false,"can_retry":false,"requires_adb_authorization_confirmation":false,"requires_usb_confirmation":false,"requires_download_confirmation":false,"backup_path":null,"last_restore":null,"updated_at":"2026-10-22T00:00:01Z","sources":[],"upstream":"rust"}"#

@main
enum CallModeMutationFenceTests {
    @MainActor
    static func main() async throws {
        let backend = BackendProcess()
        let store = CallModeStore(backend: backend)

        UnixSocketURLProtocol.configure([
            .failure(.timedOut),
            .response(200, busyStatus),
        ])
        store.enable(confirmADBAuthorization: true)
        try await waitUntil { UnixSocketURLProtocol.requests.count == 2 && store.status?.isBusy == true }
        try expect(store.actionInFlight, "unknown mutation receipt released the call-mode fence")
        try expect(UnixSocketURLProtocol.requests.map(\.httpMethod) == ["POST", "GET"],
                   "unknown mutation was replayed instead of reconciling status")
        print("PASS: lost mutation receipt keeps the fence and performs one Rust status reconciliation")

        UnixSocketURLProtocol.configure([.response(200, readyStatus)])
        store.poll()
        try await waitUntil { store.status?.isReady == true && !store.actionInFlight }
        try expect(UnixSocketURLProtocol.requests.count == 1 && UnixSocketURLProtocol.requests[0].httpMethod == "GET",
                   "Rust terminal projection did not release the fence through a single read")
        print("PASS: only a Rust non-busy projection releases the call-mode action fence")

        UnixSocketURLProtocol.configure([
            .response(400, #"{"error":"rejected","code":"fixture_rejected","recoverable":false}"#),
        ])
        store.retryPreparation()
        try await waitUntil { store.error?.contains("rejected") == true }
        try expect(!store.actionInFlight, "explicit Rust rejection retained an unnecessary mutation fence")
        try expect(UnixSocketURLProtocol.requests.count == 1, "explicit rejection triggered hidden retry or polling")
        print("PASS: explicit Rust rejection is terminal without replay")
    }
}
