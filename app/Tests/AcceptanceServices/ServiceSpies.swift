import Foundation

// In-memory boundaries for actual UpdateChecker/AutoLaunch production methods.
// The compiled test has no route to a real URLSession, preference store or login item.
@MainActor final class AcceptanceFixtureDefaults {
    static let standard = AcceptanceFixtureDefaults()
    var values: [String: Any] = [:]
    var reads = 0
    var writes = 0
    func object(forKey key: String) -> Any? { reads += 1; return values[key] }
    func string(forKey key: String) -> String? { object(forKey: key) as? String }
    func bool(forKey key: String) -> Bool { object(forKey: key) as? Bool ?? false }
    func stringArray(forKey key: String) -> [String]? { object(forKey: key) as? [String] }
    func set(_ value: Any?, forKey key: String) { writes += 1; values[key] = value }
}
@MainActor final class AcceptanceFixtureURLSession {
    static let shared = AcceptanceFixtureURLSession()
    var requests = 0
    func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        requests += 1
        throw URLError(.notConnectedToInternet)
    }
}
@MainActor final class AcceptanceFixtureLoginService {
    enum Status { case enabled, notRegistered }
    static let mainApp = AcceptanceFixtureLoginService()
    var observations = 0
    var mutations = 0
    var status: Status { observations += 1; return .enabled }
    func register() throws { mutations += 1 }
    func unregister() throws { mutations += 1 }
}
@MainActor final class AcceptanceFixtureFiles {
    static let `default` = AcceptanceFixtureFiles()
    var calls = 0
    func fileExists(atPath path: String) -> Bool { calls += 1; return false }
    func removeItem(atPath path: String) throws { calls += 1 }
    static func homeDirectory() -> String { "/fixture-never-write" }
    static func write(_ data: Data, to path: String) throws { `default`.calls += 1 }
}
@MainActor final class AcceptanceFixtureProcess {
    var executableURL: URL?
    var arguments: [String]?
    var terminationStatus: Int32 { 0 }
    func run() throws { AcceptanceFixtureFiles.default.calls += 1 }
    func waitUntilExit() {}
}
