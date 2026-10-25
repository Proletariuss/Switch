import Foundation

protocol ObservableObject {}
@MainActor final class BackendProcess {
    enum State { case running, stopped }
    static let shared = BackendProcess()
    var state: State = .running
}
@MainActor final class AttentionStore {
    static let shared = AttentionStore()
    var received = 0
    func recordIncomingSMS(count: Int, isVisible: Bool) { if !isVisible { received += count } }
    var viewingThreadID: String?
    var receivedIDs = Set<String>()
    func recordIncomingSMS(messageID: String, threadID: String) {
        if !isViewingSMS(threadID: threadID), receivedIDs.insert(messageID).inserted { received += 1 }
    }
    func isViewingSMS(threadID: String) -> Bool { NSApp.isActive && viewingThreadID == threadID }
    func forgetSMS(messageID: String) { if receivedIDs.remove(messageID) != nil { received -= 1 } }
    func markSMSThreadViewed(_ threadID: String) {}
}
enum PrivacyMask { static func value(_ value: String, enabled: Bool) -> String { enabled ? "masked" : value } }
enum AppPreferenceKeys { static let privacyMaskEnabled = "privacy" }
@MainActor enum NSApp { static var isActive = false }
func userFacingErrorMessage(_ error: Error) -> String { error.localizedDescription }
func isModuleDisconnectedError(_ message: String) -> Bool { message.contains("not connected") }

struct DeviceSMSForwardingHandoff: Equatable {
    let acceptedNotificationIDs: [String]
    let wasEnabled: Bool?
}
@MainActor final class SMSTestDefaults {
    static let shared = SMSTestDefaults()
    var values: [String: Any] = [:]
    func object(forKey key: String) -> Any? { values[key] }
    func string(forKey key: String) -> String? { values[key] as? String }
    func stringArray(forKey key: String) -> [String]? { values[key] as? [String] }
    func bool(forKey key: String) -> Bool { values[key] as? Bool ?? false }
    func set(_ value: Any, forKey key: String) { values[key] = value }
}

enum UNAuthorizationStatus { case authorized, provisional, ephemeral, notDetermined, denied }
enum UNNotificationSetting { case enabled, disabled }
struct UNNotificationSettings {
    var authorizationStatus: UNAuthorizationStatus = .authorized
    var alertSetting: UNNotificationSetting = .enabled
}
struct UNAuthorizationOptions: OptionSet {
    let rawValue: Int
    static let alert = Self(rawValue: 1)
    static let sound = Self(rawValue: 2)
}
enum UNNotificationSound { case `default` }
final class UNMutableNotificationContent {
    var title = ""
    var body = ""
    var sound: UNNotificationSound?
    var userInfo: [String: String] = [:]
}
struct UNNotificationRequest {
    let identifier: String
    let content: UNMutableNotificationContent
    let trigger: String?
}
@MainActor final class UNUserNotificationCenter {
    static let shared = UNUserNotificationCenter()
    static var currentCalls = 0
    static func current() -> UNUserNotificationCenter { currentCalls += 1; return shared }
    var posts: [UNNotificationRequest] = []
    var holdSettings = false
    var settingsWaiter: CheckedContinuation<UNNotificationSettings, Never>?
    func notificationSettings() async -> UNNotificationSettings {
        if holdSettings {
            return await withCheckedContinuation { settingsWaiter = $0 }
        }
        return UNNotificationSettings()
    }
    func requestAuthorization(options: UNAuthorizationOptions) async throws -> Bool { true }
    func add(_ request: UNNotificationRequest) { posts.append(request) }
}

@MainActor final class SMSTestAPI {
    static let shared = SMSTestAPI()
    var defaults: [String: String] = [:]
    var heldRoutes: Set<String> = []
    var requests: [String] = []
    var bodies: [String: [Data]] = [:]
    var waiters: [String: [CheckedContinuation<Data, Error>]] = [:]
    func reset() {
        precondition(waiters.values.allSatisfy(\.isEmpty), "unresolved test request")
        defaults = [
            "GET api/sms/status": "{}",
            "GET api/sms/devices": "{\"devices\":[]}",
            "GET api/sms": "[]",
            "GET api/sms/storage": "{}",
            "POST api/sms/refresh": "{\"accepted\":true}",
            "POST api/sms/delete": "{\"deleted\":true}",
            "POST api/sms/delete-sender": "{\"deleted\":true,\"deleted_count\":1}"
        ]
        heldRoutes = []
        requests = []
        bodies = [:]
    }
    func request(_ route: String) async throws -> Data {
        requests.append(route)
        if heldRoutes.contains(route) {
            // Deliberately ignore cancellation: late IPC completions must be
            // rejected by the store's generation check, not just URLSession.
            return try await withCheckedThrowingContinuation { waiters[route, default: []].append($0) }
        }
        guard let json = defaults[route] else { fatalError("unexpected request: \(route)") }
        return Data(json.utf8)
    }
    func resolve(_ route: String, json: String) {
        precondition(!(waiters[route] ?? []).isEmpty, "no held request: \(route)")
        waiters[route]!.removeFirst().resume(returning: Data(json.utf8))
    }
    func reject(_ route: String) {
        precondition(!(waiters[route] ?? []).isEmpty, "no held request: \(route)")
        waiters[route]!.removeFirst().resume(throwing: URLError(.timedOut))
    }
    func count(_ route: String) -> Int { requests.filter { $0 == route }.count }
}

@MainActor struct APIClient {
    func get<T: Decodable>(_ path: String) async throws -> T { try await read("GET " + path) }
    func send<T: Decodable>(_ path: String) async throws -> T { try await read("POST " + path) }
    func send<T: Decodable, B: Encodable>(_ path: String, body: B) async throws -> T {
        SMSTestAPI.shared.bodies["POST " + path, default: []].append(try JSONEncoder().encode(body))
        return try await read("POST " + path)
    }
    func send<B: Encodable>(_ path: String, body: B) async throws {
        let _ = try await SMSTestAPI.shared.request("POST " + path)
    }
    private func read<T: Decodable>(_ route: String) async throws -> T {
        let data = try await SMSTestAPI.shared.request(route)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        return try decoder.decode(T.self, from: data)
    }
}
