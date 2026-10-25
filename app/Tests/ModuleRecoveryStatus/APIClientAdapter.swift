import Foundation

// The live API is never called by this in-memory fixture.
struct APIClient {
    init(timeoutInterval: TimeInterval) {}
    func get<T: Decodable>(_ path: String) async throws -> T {
        throw NSError(domain: "ModuleRecoveryStatusFixture", code: 1)
    }
}
