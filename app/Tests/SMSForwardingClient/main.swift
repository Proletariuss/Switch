import Darwin
import Foundation
import CoreFoundation

protocol ObservableObject {}
typealias OSStatus = Int32
let errSecDuplicateItem: OSStatus = -25299
let errSecSuccess: OSStatus = 0
let errSecItemNotFound: OSStatus = -25300
let errSecInteractionNotAllowed: OSStatus = -25308
let errSecAuthFailed: OSStatus = -25293
let errSecDecode: OSStatus = -26275
let kSecClass = "class", kSecClassGenericPassword = "generic", kSecAttrService = "service"
let kSecAttrAccount = "account", kSecReturnData = "returnData", kSecMatchLimit = "matchLimit"
let kSecReturnAttributes = "returnAttributes", kSecMatchLimitAll = "all"
let kSecMatchLimitOne = "one", kSecValueData = "valueData", kSecAttrAccessible = "accessible"
let kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly = "afterFirstUnlock", kSecRandomDefault = 0

// Actual production read/write adapters call these local OS boundaries.
// Security is not imported, and no real Keychain/provider is accessed.
enum ForwardingKeychain {
    static let lock = NSRecursiveLock()
    static var primaryService = ""
    static let barrierLock = NSLock()
    static var reachedBarrier = ""
    static func didReach(_ phase: String) -> Bool {
        barrierLock.lock(); defer { barrierLock.unlock() }
        return reachedBarrier == phase
    }
    enum Reply { case status(OSStatus), data(Data), malformed }
    static var values: [String: Data] = [:]
    static var replies: [String: Reply] = [:]
    static var writes: [String] = []
    static var randomCalls = 0
    static var failRandom = false
    static var failWriteAt: Int?
    static var failAfterWrite = false
    static var failVerifyAt: Int?
    static var failEnumeration = false
    static var enumerationCalls = 0
    static var failEnumerationAt: Int?
    static var failDeletion = false
    static var beforeEnumeration: (() -> Void)?
    static var barrierPhases: [String] = []
    @MainActor static func reset(_ next: [String: Data] = [:]) {
        lock.lock(); defer { lock.unlock() }
        primaryService = ForwardingFixture.primaryService
        barrierLock.lock(); reachedBarrier = ""; barrierLock.unlock()
        values = next; replies = [:]; writes = []; randomCalls = 0; failRandom = false
        failWriteAt = nil; failAfterWrite = false; failVerifyAt = nil
        failEnumeration = false; enumerationCalls = 0; failEnumerationAt = nil
        failDeletion = false; beforeEnumeration = nil; barrierPhases = []
    }
    static func account(_ query: CFDictionary) -> String {
        let service = (query as NSDictionary)[kSecAttrService] as! String
        let account = (query as NSDictionary)[kSecAttrAccount] as! String
        if ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CREDENTIAL_SECRET_DIRECTORY"] != nil {
            return service + "|" + account
        }
        if service == ForwardingKeychain.primaryService { return account }
        if service == SMSForwardingCredentialScope.legacyService {
            return account.hasPrefix("configuration.") ? "legacy." + account : account
        }
        precondition(service.hasPrefix(SMSForwardingCredentialScope.legacyService + ".root.v2."))
        return service + "|" + account
    }
    static func secretFile(_ account: String) -> URL? {
        ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CREDENTIAL_SECRET_DIRECTORY"]
            .map { URL(fileURLWithPath: $0).appendingPathComponent(SMSForwardingCredentialScope.digest(account)) }
    }
    static func value(_ account: String) -> Data? {
        if let file = secretFile(account) { return try? Data(contentsOf: file) }
        return values[account]
    }
    static func setValue(_ bytes: Data?, for account: String) {
        if let file = secretFile(account) {
            let metadata = file.appendingPathExtension("account")
            if let bytes {
                try! bytes.write(to: file, options: .atomic)
                try! Data(account.utf8).write(to: metadata, options: .atomic)
            } else {
                try? FileManager.default.removeItem(at: file)
                try? FileManager.default.removeItem(at: metadata)
            }
        } else { values[account] = bytes }
    }
    static func insertValue(_ bytes: Data, for account: String) -> Bool {
        if let file = secretFile(account) {
            let fd = open(file.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
            if fd < 0 { precondition(errno == EEXIST); return false }
            defer { close(fd) }
            let wrote = bytes.withUnsafeBytes { write(fd, $0.baseAddress!, $0.count) }
            precondition(wrote == bytes.count && fsync(fd) == 0)
            try! Data(account.utf8).write(to: file.appendingPathExtension("account"), options: .atomic)
            return true
        }
        guard values[account] == nil else { return false }
        values[account] = bytes
        return true
    }
    static func accounts(service: String) -> [String] {
        precondition(service != SMSForwardingCredentialScope.legacyService
                     || ProcessInfo.processInfo.environment["SWITCH_FORWARDING_RECOVERY_MODE"] != nil,
                     "GC queried legacy namespace outside explicit recovery proof")
        if let directory = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CREDENTIAL_SECRET_DIRECTORY"] {
            return (try! FileManager.default.contentsOfDirectory(at: URL(fileURLWithPath: directory), includingPropertiesForKeys: nil))
                .filter { $0.pathExtension == "account" }
                .compactMap { try? String(contentsOf: $0, encoding: .utf8) }
                .filter { $0.hasPrefix(service + "|") }.map { String($0.dropFirst(service.count + 1)) }
        }
        if service == ForwardingKeychain.primaryService {
            return values.keys.filter { !$0.contains("|") && !$0.hasPrefix("legacy.") }
        }
        return values.keys.filter { $0.hasPrefix(service + "|") }.map { String($0.dropFirst(service.count + 1)) }
    }
    static func barrier(_ phase: String) {
        barrierLock.lock(); reachedBarrier = phase; barrierLock.unlock()
        guard barrierPhases.first == phase else { return }
        barrierPhases.removeFirst()
        FileHandle.standardOutput.write(Data((phase + "\n").utf8))
        precondition(FileHandle.standardInput.readData(ofLength: 1).count == 1)
    }
    static func writeResult() -> OSStatus {
        writes.count == failWriteAt ? errSecAuthFailed : errSecSuccess
    }
}
func SecItemCopyMatching(_ query: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>) -> OSStatus {
    ForwardingKeychain.lock.lock(); defer { ForwardingKeychain.lock.unlock() }
    if (query as NSDictionary)[kSecMatchLimit] as? String == kSecMatchLimitAll {
        ForwardingKeychain.enumerationCalls += 1
        if ForwardingKeychain.failEnumeration || ForwardingKeychain.enumerationCalls == ForwardingKeychain.failEnumerationAt {
            return errSecAuthFailed
        }
        ForwardingKeychain.beforeEnumeration?()
        let service = (query as NSDictionary)[kSecAttrService] as! String
        result.pointee = ForwardingKeychain.accounts(service: service).map { [kSecAttrAccount: $0] } as CFArray
        return errSecSuccess
    }
    let account = ForwardingKeychain.account(query)
    if let reply = ForwardingKeychain.replies[account] {
        switch reply {
        case let .status(status): return status
        case let .data(data): result.pointee = data as CFData; return errSecSuccess
        case .malformed: result.pointee = "not-data" as CFString; return errSecSuccess
        }
    }
    guard let bytes = ForwardingKeychain.value(account) else { return errSecItemNotFound }
    result.pointee = bytes as CFData
    return errSecSuccess
}
func SecItemDelete(_ query: CFDictionary) -> OSStatus {
    ForwardingKeychain.lock.lock(); defer { ForwardingKeychain.lock.unlock() }
    let account = ForwardingKeychain.account(query)
    ForwardingKeychain.writes.append("delete:" + account)
    ForwardingKeychain.barrier("delete")
    if ForwardingKeychain.failDeletion { return errSecAuthFailed }
    let existed = ForwardingKeychain.value(account) != nil
    ForwardingKeychain.setValue(nil, for: account)
    return existed ? errSecSuccess : errSecItemNotFound
}
func SecItemUpdate(_ query: CFDictionary, _ attributes: CFDictionary) -> OSStatus {
    ForwardingKeychain.lock.lock(); defer { ForwardingKeychain.lock.unlock() }
    let account = ForwardingKeychain.account(query)
    guard ForwardingKeychain.value(account) != nil else { return errSecItemNotFound }
    ForwardingKeychain.writes.append("update:" + account)
    if ForwardingKeychain.writeResult() != errSecSuccess && !ForwardingKeychain.failAfterWrite {
        return errSecAuthFailed
    }
    ForwardingKeychain.setValue((attributes as NSDictionary)[kSecValueData] as? Data, for: account)
    ForwardingKeychain.barrier(account.hasSuffix("configuration.active.v1") ? "active" : "stage")
    if ForwardingKeychain.writes.count == ForwardingKeychain.failVerifyAt {
        ForwardingKeychain.replies[account] = .status(errSecAuthFailed)
    }
    return ForwardingKeychain.writeResult()
}
func SecItemAdd(_ query: CFDictionary, _ result: UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus {
    ForwardingKeychain.lock.lock(); defer { ForwardingKeychain.lock.unlock() }
    let account = ForwardingKeychain.account(query)
    ForwardingKeychain.writes.append("add:" + account)
    if ForwardingKeychain.writeResult() != errSecSuccess && !ForwardingKeychain.failAfterWrite {
        return errSecAuthFailed
    }
    guard let bytes = (query as NSDictionary)[kSecValueData] as? Data,
          ForwardingKeychain.insertValue(bytes, for: account) else { return errSecDuplicateItem }
    ForwardingKeychain.barrier(account.hasSuffix("configuration.active.v1") ? "active" : "stage")
    if ForwardingKeychain.writes.count == ForwardingKeychain.failVerifyAt {
        ForwardingKeychain.replies[account] = .status(errSecAuthFailed)
    }
    return ForwardingKeychain.writeResult()
}
func SecRandomCopyBytes(_ source: Int, _ count: Int, _ bytes: UnsafeMutableRawPointer) -> OSStatus {
    ForwardingKeychain.lock.lock(); defer { ForwardingKeychain.lock.unlock() }
    ForwardingKeychain.randomCalls += 1
    if ForwardingKeychain.failRandom { return errSecAuthFailed }
    let seed = Array(SMSForwardingCredentialScope.digest(String(getpid()) + "/" + String(ForwardingKeychain.randomCalls)).utf8)
    precondition(count <= seed.count)
    seed.withUnsafeBytes { bytes.copyMemory(from: $0.baseAddress!, byteCount: count) }
    return errSecSuccess
}
@MainActor final class BackendProcess {
    enum State { case running, stopped }
    static let shared = BackendProcess()
    var state = State.running
}
struct APIError: Error { let statusCode: Int }
func userFacingErrorMessage(_ error: Error) -> String { "fixture failure" }

@MainActor enum SwitchRuntimePaths {
    static func dataDirectory() -> URL { ForwardingFixture.checkpointDirectory }
}

@MainActor enum ForwardingFixture {
    static func discardEmptyQueueAuthorityForTest() {
        let directory = (alternateRoot ?? checkpointDirectory).appendingPathComponent("sms-forwarding")
        let queue = directory.appendingPathComponent("state.enc")
        if FileManager.default.fileExists(atPath: queue.path) {
            let wire = try! Data(contentsOf: queue)
            precondition(wire.count == 56 && wire.starts(with: Data("SWF1".utf8)),
                         "test reset may discard only the synthetic empty SWF1 seed")
        }
        if FileManager.default.fileExists(atPath: directory.path) {
            try! FileManager.default.removeItem(at: directory)
        }
    }

    static func protocolRoot(_ name: String) -> URL {
        let root = checkpointDirectory.appendingPathComponent("protocol-" + name + "-" + UUID().uuidString)
        try! FileManager.default.createDirectory(at: root, withIntermediateDirectories: false,
                                                 attributes: [.posixPermissions: 0o700])
        return root
    }
    static let persistenceFailureOnly = ProcessInfo.processInfo.environment[
        "SWITCH_FORWARDING_HANDOFF_SYNC_FAILURE_ONLY"] == "1"
    static let domain = "Switch-ForwardingClient-" + UUID().uuidString
    static let defaults = UserDefaults(suiteName: domain)!
    static let checkpointDirectory: URL = {
        guard let buildPath = ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"] else {
            fatalError("a workspace build root is required")
        }
        let build = URL(fileURLWithPath: buildPath).resolvingSymlinksInPath().standardizedFileURL
        let requested = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CHECKPOINT_DIR"]
        let directory = requested.map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? build.appendingPathComponent("tmp/forwarding-checkpoint-" + UUID().uuidString, isDirectory: true)
        precondition(directory.isFileURL && directory.path.hasPrefix("/"))
        precondition(directory.standardizedFileURL.resolvingSymlinksInPath().path.hasPrefix(build.path + "/"),
                     "checkpoint root must belong to Switch/build before any creation")
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        precondition(directory.resolvingSymlinksInPath().path.hasPrefix(build.path + "/"))
        return directory
    }()
    static var alternateRoot: URL?
    static var alternateLockHome: URL?
    static let primaryService = SMSForwardingCredentialScope.legacyService + ".root.v2."
        + SMSForwardingCredentialScope.digest(checkpointDirectory.resolvingSymlinksInPath().path)
    static let credentialHome: URL = {
        let home = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CREDENTIAL_HOME"]
            .map { URL(fileURLWithPath: $0) }
            ?? checkpointDirectory.appendingPathComponent("credential-fixture-home")
        try! FileManager.default.createDirectory(at: home, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        return home
    }()
    static func credentialScope() throws -> SMSForwardingCredentialScope {
        try SMSForwardingCredentialScope(dataRoot: alternateRoot ?? checkpointDirectory,
            defaultRoot: ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CREDENTIAL_DEFAULT_ROOT"]
                .map { URL(fileURLWithPath: $0) } ?? checkpointDirectory,
            lockHome: alternateLockHome ?? credentialHome)
    }
    static var afterHandoffRead: (() -> Void)?
    static var liveRequests: [String] = []
    static var calls: [String] = []
    static var omitConfigurationID = false
    static var failHandoffOnce = false
    static var holdConfigure = false
    static var rejectConfigure = false
    static var activeTelegramEnabled = false
    static var activeConfigurationID: String?
    static var telegramTestCalls = 0
    static var configureBodies: [[String: Any]] = []
    static var completedConfigurations: [[String: Any]] = []
    static var testBodies: [[String: Any]] = []
    static var testReceipts: [String: Data] = [:]
    static var dropTestResponseOnce = false
    static var unknownTestOnce = false
    static var holdTest = false
    static var heldRoute: String?
    static var routeWaiters: [(CheckedContinuation<Data, Error>, Data)] = []
    static func response(_ route: String, data: Data) async throws -> Data {
        if heldRoute == route {
            return try await withCheckedThrowingContinuation { routeWaiters.append(($0, data)) }
        }
        return data
    }
    static func releaseRoute() {
        let (waiter, data) = routeWaiters.removeFirst()
        heldRoute = nil
        waiter.resume(returning: data)
    }
    static var testWaiters: [(CheckedContinuation<Data, Error>, Data)] = []
    static var handoffRequests: [[String: Any]] = []
    static var acceptedHandoff: [String: Any]?
    static var waiters: [CheckedContinuation<Data, Error>] = []
    static func configurationID(_ body: [String: Any]) -> String {
        let sequence = (body["client_sequence"] as! NSNumber).uint64Value
        return String(format: "%032llx", sequence)
    }
    static func configurationResponse(_ body: [String: Any]) -> Data {
        var response: [String: Any] = [
            "configured": true,
            "client_request_id": body["client_request_id"] as! String,
            "client_sequence": (body["client_sequence"] as! NSNumber).uint64Value,
            "configuration_intent_revision": (body["client_sequence"] as! NSNumber).uint64Value,
        ]
        if !omitConfigurationID { response["configuration_id"] = configurationID(body) }
        return try! JSONSerialization.data(withJSONObject: response)
    }
    static func statusData() -> Data {
        var value: [String: Any] = [
            "configured": true, "telegram_enabled": true,
            "bark_enabled": false, "pending": 0,
        ]
        if let activeConfigurationID { value["configuration_id"] = activeConfigurationID }
        return try! JSONSerialization.data(withJSONObject: value)
    }
    static func request(_ route: String, body: Data? = nil) async throws -> Data {
        calls.append(route)
        if route == "GET api/v2/sms/forwarding/handoff" {
            afterHandoffRead?()
            return try await response(route, data: JSONSerialization.data(withJSONObject: [
                "accepted": acceptedHandoff != nil,
                "snapshot": (acceptedHandoff as Any?) ?? NSNull(),
            ]))
        }
        if route == "POST api/sms/forwarding/configure" {
            if rejectConfigure { throw APIError(statusCode: 400) }
            let value = try! JSONSerialization.jsonObject(with: body!) as! [String: Any]
            configureBodies.append(value)
            if holdConfigure { return try await withCheckedThrowingContinuation { waiters.append($0) } }
            configureBodies.removeLast()
            completedConfigurations.append(value)
            activeTelegramEnabled = true
            activeConfigurationID = configurationID(value)
            return try await response(route, data: configurationResponse(value))
        }
        if route == "POST api/sms/forwarding/test" {
            let value = try! JSONSerialization.jsonObject(with: body!) as! [String: Any]
            testBodies.append(value)
            let id = value["client_request_id"] as! String
            // Mock Rust ledger survives GUI recreation. A replay must happen
            // before checking the currently installed configuration.
            if let receipt = testReceipts[id] { return receipt }
            guard activeTelegramEnabled else { throw APIError(statusCode: 400) }
            guard value["expected_configuration_id"] as? String == activeConfigurationID else {
                throw APIError(statusCode: 409)
            }
            telegramTestCalls += 1
            let unknown = unknownTestOnce
            unknownTestOnce = false
            let receipt = try JSONSerialization.data(withJSONObject: [
                "client_request_id":id, "provider":value["provider"]!,
                "expected_configuration_id":value["expected_configuration_id"]!,
                "state":unknown ? "unknown_no_replay" : "succeeded",
                "terminal":!unknown, "sent":!unknown, "dispatch_count":1,
                "code":unknown ? "sms_forwarding_test_result_unknown" : "",
            ])
            testReceipts[id] = receipt
            if holdTest {
                return try await withCheckedThrowingContinuation { testWaiters.append(($0, receipt)) }
            }
            if dropTestResponseOnce {
                dropTestResponseOnce = false
                throw APIError(statusCode: 503)
            }
            return try await response(route, data: receipt)
        }
        if route == "GET api/sms/forwarding/status" {
            return try await response(route, data: statusData())
        }
        if route == "POST api/sms/forwarding/queue-action" {
            return try await response(route, data: Data("{\"accepted\":true}".utf8))
        }
        fatalError("unexpected request: \(route)")
    }
    static func resolve(_ index: Int = 0) {
        activeTelegramEnabled = true
        let body = configureBodies.remove(at: index)
        activeConfigurationID = configurationID(body)
        waiters.remove(at: index).resume(returning: configurationResponse(body))
    }
    static func reject(_ index: Int = 0, status: Int = 409) {
        configureBodies.remove(at: index)
        waiters.remove(at: index).resume(throwing: APIError(statusCode: status))
    }
}
@MainActor struct APIClient {
    func get<T: Decodable>(_ path: String, as type: T.Type = T.self) async throws -> T {
        if ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET"] != nil {
            let (status, data) = try UnixHandoff.request(method: "GET", path: "/" + path)
            ForwardingFixture.liveRequests.append("GET " + path)
            guard (200..<300).contains(status) else { throw APIError(statusCode: status) }
            return try JSONDecoder().decode(T.self, from: data)
        }
        return try JSONDecoder().decode(T.self, from: await ForwardingFixture.request("GET " + path))
    }
    func send<T: Decodable, B: Encodable>(_ path: String, body: B, as type: T.Type = T.self) async throws -> T {
        if ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET"] != nil {
            let (status, data) = try UnixHandoff.request(method: "POST", path: "/" + path,
                                                       body: JSONEncoder().encode(body))
            ForwardingFixture.liveRequests.append("POST " + path)
            guard (200..<300).contains(status) else { throw APIError(statusCode: status) }
            return try JSONDecoder().decode(T.self, from: data)
        }
        if path == "api/v2/sms/forwarding/handoff" {
            let data = try JSONEncoder().encode(body)
            let value = try JSONSerialization.jsonObject(with: data) as! [String: Any]
            ForwardingFixture.handoffRequests.append(value)
            if ForwardingFixture.failHandoffOnce {
                ForwardingFixture.failHandoffOnce = false
                throw APIError(statusCode: 503)
            }
            let alreadyAccepted = ForwardingFixture.acceptedHandoff != nil
            ForwardingFixture.acceptedHandoff = value
            let response = try JSONSerialization.data(withJSONObject: [
                "accepted": true,
                "handoff_id": value["handoff_id"] as! String,
                "already_accepted": alreadyAccepted,
            ])
            return try JSONDecoder().decode(T.self, from: await ForwardingFixture.response(
                "POST " + path, data: response))
        }
        return try JSONDecoder().decode(T.self, from: await ForwardingFixture.request(
            "POST " + path, body: JSONEncoder().encode(body)))
    }
}

@main @MainActor enum ForwardingClientTests {
    static func until(_ condition: () -> Bool) async {
        for _ in 0..<2000 { if condition() { return }; await Task.yield() }
        fatalError("test condition did not settle")
    }

    static func statusReadsStayFenced() async {
        let root = ForwardingFixture.protocolRoot("status-read-generation")
        ForwardingFixture.alternateRoot = root
        defer {
            ForwardingFixture.alternateRoot = nil
            ForwardingFixture.heldRoute = nil
            ForwardingFixture.holdConfigure = false
            BackendProcess.shared.state = .running
            try! FileManager.default.removeItem(at: root)
        }
        let store = SMSForwardingStore()
        precondition(store.credentialLoadFailure == nil)
        store.admissionHandoffSubmitted = true
        store.telegramEnabled = true
        await store.save()
        precondition(store.errorMessage == nil && store.status?.configurationID != nil)

        for lateFailure in [false, true] {
            ForwardingFixture.heldRoute = "GET api/sms/forwarding/status"
            let old = Task { await store.refreshStatus() }
            await until { ForwardingFixture.routeWaiters.count == 1 }
            ForwardingFixture.heldRoute = nil
            store.telegramChatID = lateFailure ? "new-failure-case" : "new-success-case"
            await store.save()
            let currentID = store.status?.configurationID
            precondition(currentID == ForwardingFixture.activeConfigurationID && store.errorMessage == nil)
            let (waiter, oldData) = ForwardingFixture.routeWaiters.removeFirst()
            if lateFailure { waiter.resume(throwing: APIError(statusCode: 503)) }
            else { waiter.resume(returning: oldData) }
            await old.value
            precondition(store.status?.configurationID == currentID && store.errorMessage == nil,
                         "ordinary stale GET overwrote the newer configuration result")
        }

        // A poll may also begin AFTER save advanced its form generation but
        // BEFORE configure finishes. Publication must invalidate that read too.
        ForwardingFixture.holdConfigure = true
        let save = Task { await store.save() }
        await until { ForwardingFixture.waiters.count == 1 }
        ForwardingFixture.heldRoute = "GET api/sms/forwarding/status"
        let duringSave = Task { await store.refreshStatus() }
        await until { ForwardingFixture.routeWaiters.count == 1 }
        ForwardingFixture.heldRoute = nil
        ForwardingFixture.holdConfigure = false
        ForwardingFixture.resolve()
        await save.value
        let savedID = store.status?.configurationID
        let (oldWaiter, oldData) = ForwardingFixture.routeWaiters.removeFirst()
        oldWaiter.resume(returning: oldData)
        await duringSave.value
        precondition(store.status?.configurationID == savedID && store.errorMessage == nil,
                     "read begun during configure displaced its verified snapshot")

        // Exercise the production backend transition method used by Combine;
        // the fixture replaces the publisher/IO, not its invalidation policy.
        ForwardingFixture.heldRoute = "GET api/sms/forwarding/status"
        let beforeRestart = Task { await store.refreshStatus() }
        await until { ForwardingFixture.routeWaiters.count == 1 }
        BackendProcess.shared.state = .stopped
        store.backendStateChanged(.stopped)
        precondition(store.status == nil && !store.isRefreshingStatus)
        BackendProcess.shared.state = .running
        store.backendStateChanged(.running)
        await until { ForwardingFixture.routeWaiters.count == 2 }
        let (restartWaiter, restartData) = ForwardingFixture.routeWaiters.remove(at: 1)
        restartWaiter.resume(returning: restartData)
        await until { store.confirmedConfigurationID != nil && store.status != nil }

        let successor = Task { await store.refreshStatus() }
        await until { ForwardingFixture.routeWaiters.count == 2 }
        let (retired, _) = ForwardingFixture.routeWaiters.removeFirst()
        retired.resume(throwing: APIError(statusCode: 503))
        await beforeRestart.value
        precondition(store.isRefreshingStatus && store.errorMessage == nil,
                     "retired backend GET changed successor busy/feedback")
        await store.refreshStatus()
        precondition(ForwardingFixture.routeWaiters.count == 1,
                     "retired defer allowed a duplicate successor poll")
        ForwardingFixture.releaseRoute()
        await successor.value
        precondition(!store.isRefreshingStatus && store.status?.configurationID == ForwardingFixture.activeConfigurationID)
        print("Forwarding status: late success/error, read during save, backend replacement and successor retirement passed")
    }

    static func persistenceFailureKeepsAdmissionPaused() async {
        precondition(geteuid() != 0, "real permission-failure test requires a non-root process")
        let directory = ForwardingFixture.checkpointDirectory
        let checkpoint = SMSForwardingCheckpoint(directory: directory)
        precondition(try! checkpoint.loadConfirmed() == nil)
        ForwardingFixture.afterHandoffRead = {
            precondition(chmod(directory.path, 0o500) == 0)
        }
        defer {
            _ = chmod(directory.path, 0o700)
            ForwardingFixture.afterHandoffRead = nil
        }
        let store = SMSForwardingStore(checkpoint: checkpoint)
        for attempt in 0..<2 {
            do {
                _ = try await store.submitAdmissionHandoff(
                    deviceAcceptedNotificationIDs: [String(repeating: attempt == 0 ? "a" : "b", count: 64)],
                    deviceWasEnabled: attempt == 0, nwaitCursor: attempt == 0 ? "opaque-cursor" : "changed-cursor",
                    nwaitSeeded: attempt == 0, nwaitWasEnabled: attempt != 0)
                fatalError("real file write failure submitted a handoff")
            } catch let error as SMSForwardingHandoffError {
                guard case .snapshotPersistenceFailed = error else { fatalError("wrong persistence error") }
            } catch { fatalError("write failure escaped as an unrelated error") }
            precondition(ForwardingFixture.handoffRequests.isEmpty && ForwardingFixture.acceptedHandoff == nil)
            precondition(ForwardingFixture.calls == ["GET api/v2/sms/forwarding/handoff"])
            let pending = try! SMSForwardingStore.decodeFrozenHandoff(store.handoffAuthority.pendingForTesting()!)
            precondition(pending.deviceSMS.acceptedNotificationIDs == [String(repeating: "a", count: 64)])
            precondition(pending.nwaitSMS.cursor == "opaque-cursor" && pending.nwaitSMS.seeded)
        }
        let original = try! store.handoffAuthority.pendingForTesting()!
        precondition(chmod(directory.path, 0o700) == 0)
        ForwardingFixture.afterHandoffRead = nil
        let recovered = try! await store.submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
            nwaitSeeded: false, nwaitWasEnabled: nil)
        let expected = try! SMSForwardingStore.decodeFrozenHandoff(original)
        precondition(recovered.handoffID == expected.handoffID)
        precondition(try! checkpoint.loadConfirmed() == original)
        precondition(ForwardingFixture.handoffRequests.count == 1)
        print("Forwarding persistence: two actual write failures sent zero POSTs; real retry retained original snapshot")
    }

    static func assertFixture(_ snapshot: SMSForwardingHandoffV1) {
        precondition(snapshot.schemaVersion == 1)
        precondition(snapshot.deviceSMS.acceptedNotificationIDs == [String(repeating: "a", count: 64)])
        precondition(snapshot.deviceSMS.wasEnabled == true && snapshot.nwaitSMS.cursor == "opaque-cursor")
        precondition(snapshot.nwaitSMS.seeded && snapshot.nwaitSMS.wasEnabled == false)
    }

    static func exportConfirmed(_ checkpoint: SMSForwardingCheckpoint) throws {
        guard let path = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH"] else {
            fatalError("independent fixture export path required")
        }
        let data = try checkpoint.loadConfirmed()!
        assertFixture(try SMSForwardingStore.decodeFrozenHandoff(data))
        let target = URL(fileURLWithPath: path).standardizedFileURL
        let build = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"]!).resolvingSymlinksInPath()
        precondition(target.deletingLastPathComponent().resolvingSymlinksInPath().path.hasPrefix(build.path + "/"))
        let descriptor = open(target.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        precondition(descriptor >= 0)
        let file = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        try file.write(contentsOf: data)
        try file.synchronize()
        try file.close()
        let parent = open(target.deletingLastPathComponent().path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        precondition(parent >= 0)
        defer { _ = close(parent) }
        precondition(fsync(parent) == 0)
    }

    static func independentMode(_ mode: String) async throws {
        let checkpoint = SMSForwardingCheckpoint(directory: ForwardingFixture.checkpointDirectory)
        if mode == "verify" {
            let output = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH"]!
            let returned = try SMSForwardingStore.decodeFrozenHandoff(Data(contentsOf: URL(fileURLWithPath: output)))
            let restored = try SMSForwardingStore.decodeFrozenHandoff(checkpoint.loadConfirmed()!)
            precondition(returned == restored)
            assertFixture(restored)
            try exportConfirmed(checkpoint)
            print("Independent Swift verifier reopened the production checkpoint and matched Rust DTO")
            return
        }
        let store = SMSForwardingStore(checkpoint: checkpoint)
        if mode == "produce" {
            let initial = try checkpoint.loadConfirmed()
            precondition(initial == nil)
            ForwardingFixture.failHandoffOnce = true
            do {
                _ = try await store.submitAdmissionHandoff(
                    deviceAcceptedNotificationIDs: [String(repeating: "a", count: 64)], deviceWasEnabled: true,
                    nwaitCursor: "opaque-cursor", nwaitSeeded: true, nwaitWasEnabled: false)
                fatalError("producer did not see its single simulated HTTP 503")
            } catch let error as APIError { precondition(error.statusCode == 503) }
            precondition(ForwardingFixture.handoffRequests.count == 1 && !ForwardingFixture.failHandoffOnce)
        } else {
            precondition(["restart", "live", "live-replay"].contains(mode), "unknown fixture mode")
            let changed = mode != "live"
            let ack = try await store.submitAdmissionHandoff(
                deviceAcceptedNotificationIDs: [String(repeating: changed ? "b" : "a", count: 64)],
                deviceWasEnabled: !changed, nwaitCursor: changed ? "changed-cursor" : "opaque-cursor",
                nwaitSeeded: !changed, nwaitWasEnabled: changed)
            let restored = try SMSForwardingStore.decodeFrozenHandoff(checkpoint.loadConfirmed()!)
            assertFixture(restored)
            precondition(ack.accepted && ack.handoffID == restored.handoffID)
            if mode == "live" {
                precondition(!ack.alreadyAccepted)
                precondition(ForwardingFixture.liveRequests == [
                    "GET api/v2/sms/forwarding/handoff", "POST api/v2/sms/forwarding/handoff"])
            }
            if mode == "live-replay" {
                precondition(ack.alreadyAccepted)
                precondition(ForwardingFixture.liveRequests == ["POST api/v2/sms/forwarding/handoff"])
            }
            if mode.hasPrefix("live") {
                let receipt = try JSONSerialization.data(withJSONObject: [
                    "mode": mode, "actual_get": mode == "live" ? 1 : 0,
                    "actual_post": 1, "accepted": ack.accepted, "already_accepted": ack.alreadyAccepted])
                print("SWIFT_LIVE_RECEIPT " + String(data: receipt, encoding: .utf8)!)
            }
        }
        try exportConfirmed(checkpoint)
        print("Independent Swift handoff mode \(mode) confirmed production checkpoint persistence")
    }

    #if SWITCH_CHECKPOINT_TESTS
    static func committedSyncFailureKeepsAdmissionPaused() async throws {
        let root = ForwardingFixture.checkpointDirectory
        let failing = SMSForwardingCheckpoint(directory: root, failing: .directorySync)
        let snapshot = SMSForwardingHandoffV1(
            schemaVersion: 1, handoffID: UUID().uuidString.lowercased(),
            deviceSMS: DeviceSMSForwardingHandoff(acceptedNotificationIDs: [String(repeating: "a", count: 64)], wasEnabled: true),
            nwaitSMS: NwaitSMSForwardingHandoff(cursor: "opaque-cursor", seeded: true, wasEnabled: false))
        let bytes = try JSONEncoder().encode(snapshot)
        do { _ = try failing.confirm(bytes); fatalError("injected committed sync failure was ignored") }
        catch SMSForwardingCheckpoint.Failure.unavailable {}
        let failingStore = SMSForwardingStore(checkpoint: failing)
        do {
            _ = try await failingStore.submitAdmissionHandoff(
                deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
                nwaitSeeded: false, nwaitWasEnabled: nil)
            fatalError("unconfirmed committed snapshot reached POST")
        } catch let failure as SMSForwardingHandoffError {
            guard case .snapshotPersistenceFailed = failure else { fatalError("wrong unconfirmed commit error") }
        }
        precondition(ForwardingFixture.handoffRequests.isEmpty && ForwardingFixture.calls.isEmpty)
        let normal = SMSForwardingCheckpoint(directory: root)
        let ack = try await SMSForwardingStore(checkpoint: normal).submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
            nwaitSeeded: false, nwaitWasEnabled: nil)
        precondition(ack.accepted && ack.handoffID == snapshot.handoffID)
        let recoveredBytes = try normal.loadConfirmed()
        precondition(recoveredBytes == bytes)
        precondition(ForwardingFixture.handoffRequests.count == 1)
        print("Committed directory-sync failure: zero POST before real re-confirmation; original snapshot recovered")
    }
    #endif

    static func durableTestReceipts() async {
        let first = SMSForwardingStore()
        let before = ForwardingFixture.telegramTestCalls
        ForwardingFixture.dropTestResponseOnce = true
        await first.test("telegram")
        let original = first.pendingTests["telegram"]!
        precondition(ForwardingFixture.telegramTestCalls == before + 1)
        precondition(first.telegramTestMessage?.contains("未知") == true)
        let reopened = SMSForwardingStore()
        precondition(reopened.pendingTests["telegram"] == original,
                     "GUI reopen must recover original request ID and entire payload")
        ForwardingFixture.activeConfigurationID = "a-different-daemon-generation"
        reopened.telegramChatID = "edited-form-must-not-change-replay"
        await reopened.test("telegram")
        precondition(reopened.pendingTests.isEmpty)
        precondition(reopened.telegramTestMessage == "测试通知已发送")
        precondition(ForwardingFixture.telegramTestCalls == before + 1)
        let replay = ForwardingFixture.testBodies.last!
        precondition(replay["client_request_id"] as? String == original.request.clientRequestID)
        precondition(replay["expected_configuration_id"] as? String == original.request.expectedConfigurationID)

        ForwardingFixture.unknownTestOnce = true
        await reopened.test("telegram")
        let unknown = reopened.pendingTests["telegram"]!
        let unknownReopen = SMSForwardingStore()
        precondition(unknownReopen.pendingTests["telegram"] == unknown)
        await unknownReopen.save() // supersedes test presentation generation
        unknownReopen.telegramTestMessage = "newer-configuration-feedback"
        await unknownReopen.test("telegram")
        precondition(unknownReopen.pendingTests["telegram"] == unknown,
                     "unknown_no_replay must keep the admission fence")
        precondition(unknownReopen.telegramTestMessage == "newer-configuration-feedback")
        precondition(ForwardingFixture.telegramTestCalls == before + 2)
        // Explicit terminal receipt is the only event that may clear a fence.
        var terminal = try! JSONSerialization.jsonObject(with:
            ForwardingFixture.testReceipts[unknown.request.clientRequestID]!) as! [String: Any]
        terminal["state"] = "failed"; terminal["terminal"] = true
        terminal["code"] = "mock_confirmed_rejection"
        ForwardingFixture.testReceipts[unknown.request.clientRequestID] =
            try! JSONSerialization.data(withJSONObject: terminal)
        await unknownReopen.test("telegram")
        precondition(unknownReopen.pendingTests.isEmpty)
        precondition(unknownReopen.telegramTestMessage == "newer-configuration-feedback")

        ForwardingFixture.holdTest = true
        let stale = Task { await unknownReopen.test("telegram") }
        await until { ForwardingFixture.testWaiters.count == 1 }
        await unknownReopen.save()
        unknownReopen.telegramTestMessage = "newer-generation-wins"
        let (waiter, receipt) = ForwardingFixture.testWaiters.removeFirst()
        ForwardingFixture.holdTest = false
        waiter.resume(returning: receipt)
        await stale.value
        precondition(unknownReopen.pendingTests.isEmpty)
        precondition(unknownReopen.telegramTestMessage == "newer-generation-wins",
                     "old terminal response overwrote a newer configuration generation")
        precondition(ForwardingFixture.telegramTestCalls == before + 3)

        // A confirmed unknown receipt permits a separately confirmed new
        // test. Rust keeps the old request ID and never dispatches it twice.
        ForwardingFixture.unknownTestOnce = true
        await unknownReopen.test("telegram")
        let oldUnknown = unknownReopen.pendingTests["telegram"]!
        let dispatchesBeforeNewTest = ForwardingFixture.telegramTestCalls
        await unknownReopen.startNewTestAfterUnknown("telegram")
        precondition(ForwardingFixture.telegramTestCalls == dispatchesBeforeNewTest + 1)
        precondition(unknownReopen.pendingTests.isEmpty)
        precondition(unknownReopen.telegramTestMessage?.contains("可能出现重复通知") == true)
        let oldReceipt = try! JSONSerialization.jsonObject(with:
            ForwardingFixture.testReceipts[oldUnknown.request.clientRequestID]!) as! [String: Any]
        precondition(oldReceipt["state"] as? String == "unknown_no_replay")
        let oldRequestCalls = ForwardingFixture.testBodies.filter {
            $0["client_request_id"] as? String == oldUnknown.request.clientRequestID
        }
        precondition(oldRequestCalls.count == 2, "new test did not query the old receipt exactly once")
        precondition(ForwardingFixture.testBodies.last?["client_request_id"] as? String
                     != oldUnknown.request.clientRequestID)

        // A lost HTTP response may hide an already successful test. The
        // explicit new action must reconcile it without a second dispatch.
        ForwardingFixture.dropTestResponseOnce = true
        await unknownReopen.test("telegram")
        let sentButUnacknowledged = unknownReopen.pendingTests["telegram"]!
        let dispatchesBeforeReconcile = ForwardingFixture.telegramTestCalls
        await unknownReopen.startNewTestAfterUnknown("telegram")
        precondition(ForwardingFixture.telegramTestCalls == dispatchesBeforeReconcile)
        precondition(unknownReopen.pendingTests.isEmpty)
        precondition(unknownReopen.telegramTestMessage == "测试通知已发送")
        precondition(ForwardingFixture.testBodies.suffix(2).allSatisfy {
            $0["client_request_id"] as? String == sentButUnacknowledged.request.clientRequestID
        })

        // A terminal provider rejection shows a safe action rather than a
        // raw backend code. No second provider dispatch follows the receipt.
        ForwardingFixture.unknownTestOnce = true
        await unknownReopen.test("telegram")
        let rejected = unknownReopen.pendingTests["telegram"]!
        var rejection = try! JSONSerialization.jsonObject(with:
            ForwardingFixture.testReceipts[rejected.request.clientRequestID]!) as! [String: Any]
        rejection["state"] = "failed"; rejection["terminal"] = true
        rejection["code"] = "telegram_http_401"
        ForwardingFixture.testReceipts[rejected.request.clientRequestID] =
            try! JSONSerialization.data(withJSONObject: rejection)
        let dispatchesBeforeFailure = ForwardingFixture.telegramTestCalls
        await unknownReopen.test("telegram")
        precondition(ForwardingFixture.telegramTestCalls == dispatchesBeforeFailure)
        precondition(unknownReopen.pendingTests.isEmpty)
        precondition(unknownReopen.telegramTestMessage?.contains("Bot Token 无效") == true)
        precondition(unknownReopen.telegramTestMessage?.contains("telegram_http_401") == false)
        print("Durable provider tests: lost response, GUI reopen, exact payload replay, unknown fence, explicit new test, stale generation, dispatch count=1 passed")
    }

    static func failurePresentationIsActionableAndRedacted() {
        let expected: [(String, String)] = [
            ("telegram_http_401", "Token"),
            ("telegram_http_403", "/start"),
            ("telegram_http_429", "频繁"),
            ("telegram_proxy_auth_required", "账号"),
            ("telegram_proxy_policy_rejected", "分流"),
            ("telegram_proxy_configuration_rejected", "代理配置"),
            ("telegram_proxy_unavailable", "代理"),
            ("telegram_connect_failed", "连接"),
            ("telegram_system_proxy_not_found", "系统代理"),
        ]
        for (code, hint) in expected {
            let message = SMSForwardingFailurePresentation.message(code, provider: "telegram")
            precondition(message.contains(hint) && !message.contains(code),
                         "Telegram provider code was hidden or leaked raw to UI")
        }
        let opaque = "telegram_secret_token_should_not_leak"
        precondition(!SMSForwardingFailurePresentation.message(opaque, provider: "telegram").contains(opaque))
        precondition(SMSForwardingFailurePresentation.message("telegram_http_502", provider: "telegram")
                     .contains("HTTP 502"))
        print("Forwarding UI: actionable Telegram failures and unknown-code redaction passed")
    }

    static func credentialReadFailuresStayFenced() async {
        let original: [String: Data] = [
            "telegram.botToken": Data("original-token".utf8),
            "telegram.proxyURL": Data("original-proxy".utf8),
            "bark.deviceKey": Data("original-device-key".utf8),
            "bark.encryptionKey": Data("original-bark-key".utf8),
            "state.encryptionKey": Data(Data(repeating: 7, count: 32).base64EncodedString().utf8),
        ]
        let faults: [(String, ForwardingKeychain.Reply)] = [
            ("telegram.botToken", .status(errSecInteractionNotAllowed)),
            ("bark.deviceKey", .status(errSecAuthFailed)),
            ("state.encryptionKey", .status(errSecInteractionNotAllowed)),
            ("bark.encryptionKey", .malformed),
            ("state.encryptionKey", .data(Data([0xff, 0xfe]))),
            ("state.encryptionKey", .status(errSecSuccess)),
        ]
        for (account, reply) in faults {
            ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
            ForwardingFixture.defaults.set("unchanged-intent", forKey: "switch.smsForwarding.configurationIntent.id")
            ForwardingFixture.defaults.set("17", forKey: "switch.smsForwarding.configurationIntent.sequence")
            ForwardingKeychain.reset(original)
            ForwardingKeychain.replies[account] = reply
            let calls = ForwardingFixture.calls.count
            let blocked = SMSForwardingStore()
            precondition(blocked.credentialLoadFailure != nil && blocked.appliedConfiguration == nil)
            precondition(blocked.errorMessage?.contains("解锁登录钥匙串后重新启动") == true)
            precondition(blocked.stateEncryptionKey.isEmpty && ForwardingKeychain.randomCalls == 0)
            precondition(blocked.telegramBotToken.isEmpty && blocked.barkDeviceKey.isEmpty,
                         "a partial OS snapshot was published")
            await blocked.synchronizeAppliedConfiguration() // same path as backend-running subscription
            blocked.telegramBotToken = "unsaved-user-edit"
            await blocked.save()
            await blocked.test("telegram")
            precondition(ForwardingFixture.calls.count == calls && ForwardingKeychain.writes.isEmpty)
            precondition(ForwardingKeychain.values == original)
            precondition(ForwardingFixture.defaults.string(forKey: "switch.smsForwarding.configurationIntent.id") == "unchanged-intent")
            precondition(ForwardingFixture.defaults.string(forKey: "switch.smsForwarding.configurationIntent.sequence") == "17")
            ForwardingKeychain.replies = [:] // OS unlocks, failed instance must not erase originals.
            await blocked.save()
            precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.values == original)
            precondition(blocked.telegramBotToken == "unsaved-user-edit", "recovery clobbered an edited form")
            let recovered = SMSForwardingStore() // explicit reopen after unlock
            precondition(recovered.credentialLoadFailure == nil)
            precondition(recovered.telegramBotToken == String(data: original["telegram.botToken"]!, encoding: .utf8))
            precondition(recovered.stateEncryptionKey == String(data: original["state.encryptionKey"]!, encoding: .utf8))
            precondition(ForwardingKeychain.randomCalls == 0)
            await recovered.save()
            for (account, value) in original {
                precondition(ForwardingKeychain.values[account] == value,
                             "Migration erased a legacy credential or rotated the state key")
            }
            precondition(SMSForwardingStore().stateEncryptionKey == recovered.stateEncryptionKey)
        }

        // A preexisting Rust receipt can still be reconciled using its original
        // durable request while OS credentials are unavailable. No configure or
        // new provider dispatch is introduced by this OS availability fence.
        ForwardingKeychain.reset(original)
        ForwardingKeychain.replies["telegram.botToken"] = .status(errSecInteractionNotAllowed)
        let replay = SMSForwardingStore()
        let request = SMSForwardingProviderRequest(clientRequestID: "retained-os-failure-test", provider: "telegram",
                                                   expectedConfigurationID: "original-config")
        replay.pendingTests["telegram"] = SMSForwardingPendingTest(request: request, configurationIntentID: "old-intent")
        ForwardingFixture.testReceipts[request.clientRequestID] = try! JSONSerialization.data(withJSONObject: [
            "client_request_id": request.clientRequestID, "provider": "telegram",
            "expected_configuration_id": "original-config", "state": "succeeded",
            "terminal": true, "sent": true, "dispatch_count": 1, "code": "",
        ])
        let dispatches = ForwardingFixture.telegramTestCalls
        let calls = ForwardingFixture.calls.count
        await replay.test("telegram")
        precondition(replay.pendingTests.isEmpty && ForwardingKeychain.writes.isEmpty)
        precondition(replay.telegramTestMessage == "测试通知已发送",
                     "resolved retained receipt still appeared pending after a Keychain failure")
        precondition(ForwardingFixture.telegramTestCalls == dispatches)
        precondition(Array(ForwardingFixture.calls.dropFirst(calls)) == ["POST api/sms/forwarding/test"])
        precondition(ForwardingFixture.testBodies.last?["client_request_id"] as? String == request.clientRequestID)

        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingKeychain.reset()
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        let fresh = SMSForwardingStore()
        precondition(fresh.credentialLoadFailure == nil && ForwardingKeychain.randomCalls == 1)
        precondition(Data(base64Encoded: fresh.stateEncryptionKey)?.count == 32)
        await fresh.save()
        precondition(SMSForwardingStore().stateEncryptionKey == fresh.stateEncryptionKey)

        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingKeychain.reset()
        let missingOriginal = SMSForwardingStore()
        precondition(missingOriginal.credentialLoadFailure != nil && ForwardingKeychain.randomCalls == 0,
                     "lost Keychain key with durable witness must remain fenced")
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        ForwardingKeychain.reset(original)
        let explicitClear = SMSForwardingStore()
        explicitClear.telegramBotToken = ""
        await explicitClear.save()
        precondition(SMSForwardingStore().telegramBotToken.isEmpty,
                     "explicit user clearing stopped working")
        precondition(ForwardingKeychain.values["telegram.botToken"] == original["telegram.botToken"],
                     "versioned migration modified the legacy account")

        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        ForwardingKeychain.reset()
        ForwardingFixture.calls = []; ForwardingFixture.testBodies = []; ForwardingFixture.configureBodies = []
        ForwardingFixture.handoffRequests = []; ForwardingFixture.acceptedHandoff = nil
        ForwardingFixture.testReceipts = [:]; ForwardingFixture.telegramTestCalls = 0
        ForwardingFixture.activeConfigurationID = nil; ForwardingFixture.activeTelegramEnabled = false
        for name in [SMSForwardingCheckpoint.fileName, SMSForwardingTestCheckpoint.fileName] {
            let file = ForwardingFixture.checkpointDirectory.appendingPathComponent(name)
            if FileManager.default.fileExists(atPath: file.path) { try! FileManager.default.removeItem(at: file) }
        }
        print("Credential OS bridge: six read failures, later unlock/reopen, original key preservation, item-not-found, explicit clear and retained Rust receipt passed")
    }

    static func configurationCommitFailuresNeverMixSnapshots() async {
        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingKeychain.reset()
        let original = SMSForwardingStore()
        original.telegramEnabled = true
        original.telegramBotToken = "old-token"
        original.telegramChatID = "old-chat"
        original.telegramProxyMode = "custom"
        original.telegramProxyURL = "socks5://old-proxy:1080"
        original.barkEnabled = true
        original.barkServerURL = "https://old.example"
        original.barkDeviceKey = "old-device"
        original.barkEncryptionKey = "old-bark-key"
        await original.save()
        precondition(original.errorMessage == nil)
        let oldKey = original.stateEncryptionKey
        let committed = ForwardingKeychain.values
        let oldIntent = original.configurationIntent
        let oldAccount = String(data: committed["configuration.active.v1"]!, encoding: .utf8)!

        let cases: [(Int?, Bool, Int?, Bool)] = [
            (1, false, nil, false), // staging write rejected
            (2, false, nil, false), // pointer write rejected
            (2, true, nil, true),   // pointer committed, response lost
            (nil, false, 1, false), // staged item readback failed
            (nil, false, 2, true),  // pointer readback failed
        ]
        for (writeFault, afterWrite, verifyFault, newIsActive) in cases {
            ForwardingKeychain.reset(committed)
            let changed = SMSForwardingStore()
            precondition(changed.configurationIntent == oldIntent)
            changed.telegramBotToken = "new-token"
            changed.telegramChatID = "new-chat"
            changed.telegramProxyURL = "socks5://new-proxy:1080"
            changed.barkServerURL = "https://new.example"
            changed.barkDeviceKey = "new-device"
            changed.barkEncryptionKey = "new-bark-key"
            ForwardingKeychain.writes = []
            ForwardingKeychain.failWriteAt = writeFault
            ForwardingKeychain.failAfterWrite = afterWrite
            ForwardingKeychain.failVerifyAt = verifyFault
            let configureCount = ForwardingFixture.calls.filter { $0 == "POST api/sms/forwarding/configure" }.count
            await changed.save()
            precondition(changed.credentialLoadFailure != nil && changed.errorMessage != nil,
                         "failed commit was not fenced")
            precondition(ForwardingFixture.calls.filter { $0 == "POST api/sms/forwarding/configure" }.count
                         == configureCount, "failed commit reached the backend")
            precondition(ForwardingKeychain.values[oldAccount] == committed[oldAccount]
                         && !ForwardingKeychain.writes.contains { $0.hasPrefix("delete:") },
                         "an unconfirmed commit reclaimed the last valid snapshot")
            let writes = ForwardingKeychain.writes.count
            await changed.save()
            precondition(ForwardingKeychain.writes.count == writes,
                         "a failed instance retried the commit")
            ForwardingKeychain.replies = [:]
            ForwardingKeychain.failWriteAt = nil
            ForwardingKeychain.failVerifyAt = nil
            let restarted = SMSForwardingStore()
            precondition(restarted.credentialLoadFailure == nil)
            precondition(restarted.telegramBotToken == (newIsActive ? "new-token" : "old-token"))
            precondition(restarted.telegramChatID == (newIsActive ? "new-chat" : "old-chat"))
            precondition(restarted.telegramProxyURL == (newIsActive ? "socks5://new-proxy:1080" : "socks5://old-proxy:1080"))
            precondition(restarted.barkServerURL == (newIsActive ? "https://new.example" : "https://old.example"))
            precondition(restarted.barkDeviceKey == (newIsActive ? "new-device" : "old-device"))
            precondition(restarted.barkEncryptionKey == (newIsActive ? "new-bark-key" : "old-bark-key"))
            precondition(restarted.stateEncryptionKey == oldKey, "state encryption key changed")
            precondition(newIsActive || restarted.configurationIntent == oldIntent)
            await restarted.synchronizeAppliedConfiguration()
            let body = ForwardingFixture.completedConfigurations.last!
            let telegram = body["telegram"] as! [String: Any]
            let bark = body["bark"] as! [String: Any]
            precondition(telegram["bot_token"] as? String == restarted.telegramBotToken
                         && bark["device_key"] as? String == restarted.barkDeviceKey
                         && body["state_key"] as? String == oldKey,
                         "backend observed a mixed configuration")
        }
        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingFixture.defaults.set(true, forKey: "switch.smsForwarding.telegram.enabled")
        ForwardingFixture.defaults.set("legacy-chat", forKey: "switch.smsForwarding.telegram.chatID")
        let legacyAccounts: [String: Data] = [
            "telegram.botToken": Data("legacy-token".utf8),
            "telegram.proxyURL": Data("legacy-proxy".utf8),
            "bark.deviceKey": Data("legacy-device".utf8),
            "bark.encryptionKey": Data("legacy-bark-key".utf8),
            "state.encryptionKey": Data(oldKey.utf8),
        ]
        ForwardingKeychain.reset(legacyAccounts)
        ForwardingKeychain.failWriteAt = 2
        let migration = SMSForwardingStore()
        migration.telegramBotToken = "edited-token"
        migration.telegramChatID = "edited-chat"
        let beforeMigration = ForwardingFixture.calls.count
        await migration.save()
        precondition(migration.credentialLoadFailure != nil && ForwardingFixture.calls.count == beforeMigration)
        precondition(ForwardingKeychain.values["configuration.active.v1"] == nil)
        for (account, value) in legacyAccounts {
            precondition(ForwardingKeychain.values[account] == value)
        }
        ForwardingKeychain.failWriteAt = nil
        let reopenedLegacy = SMSForwardingStore()
        precondition(reopenedLegacy.telegramBotToken == "legacy-token"
                     && reopenedLegacy.telegramChatID == "legacy-chat"
                     && reopenedLegacy.stateEncryptionKey == oldKey)
        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        ForwardingKeychain.reset()
        print("Forwarding client: staged write/readback faults and failed legacy migration stayed atomic across restart")
    }

    static func bootstrapAndBoundedReclamationStayDurable() async {
        let active = "configuration.active.v1"
        let prefix = "configuration.snapshot.v1."
        let intentID = "switch.smsForwarding.configurationIntent.id"
        let intentSequence = "switch.smsForwarding.configurationIntent.sequence"
        func clearDefaults() {
            ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        }
        clearDefaults()
        ForwardingKeychain.reset()
        ForwardingKeychain.failRandom = true
        let beforeRandomFailure = ForwardingFixture.calls.count
        let randomFailed = SMSForwardingStore()
        precondition(randomFailed.credentialLoadFailure != nil && randomFailed.appliedConfiguration == nil)
        await randomFailed.save(); await randomFailed.test("telegram")
        precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.values.isEmpty
                     && ForwardingFixture.calls.count == beforeRandomFailure,
                     "random-source failure wrote, uploaded or dispatched")
        ForwardingKeychain.reset()
        let first = SMSForwardingStore()
        let firstKey = first.stateEncryptionKey
        let firstIntent = first.configurationIntent
        precondition(first.appliedConfiguration != nil && ForwardingKeychain.writes.count == 2)
        let committed = ForwardingKeychain.values
        let firstAccount = String(data: committed[active]!, encoding: .utf8)!
        let stored = try! JSONDecoder().decode(SMSForwardingStoredConfiguration.self,
                                               from: committed[firstAccount]!)
        precondition(stored.configuration.stateKey == firstKey && stored.intent == firstIntent)
        precondition(ForwardingFixture.defaults.string(forKey: intentID) == nil,
                     "bootstrap persisted an intent separately from its key")
        // Reopen before any save or configure, as after an immediate process exit.
        let reopened = SMSForwardingStore()
        precondition(reopened.stateEncryptionKey == firstKey && reopened.configurationIntent == firstIntent)
        precondition(ForwardingKeychain.randomCalls == 1 && ForwardingKeychain.writes.count == 2)
        await reopened.synchronizeAppliedConfiguration()
        precondition(ForwardingFixture.completedConfigurations.last?["state_key"] as? String == firstKey)

        let faults: [(Int?, Bool, Int?, Bool)] = [
            (1, false, nil, false), (2, false, nil, false),
            (2, true, nil, true), (nil, false, 1, false), (nil, false, 2, true),
        ]
        for (writeFault, afterWrite, verifyFault, committedPointer) in faults {
            clearDefaults()
            ForwardingFixture.discardEmptyQueueAuthorityForTest()
            ForwardingFixture.defaults.set("memory-only-legacy-intent", forKey: intentID)
            ForwardingFixture.defaults.set("41", forKey: intentSequence)
            ForwardingFixture.defaults.set(true, forKey: "switch.smsForwarding.telegram.enabled")
            ForwardingKeychain.reset(["telegram.botToken": Data("legacy-token".utf8)])
            ForwardingKeychain.failWriteAt = writeFault
            ForwardingKeychain.failAfterWrite = afterWrite
            ForwardingKeychain.failVerifyAt = verifyFault
            let calls = ForwardingFixture.calls.count
            let failed = SMSForwardingStore()
            let attemptedKey = ForwardingKeychain.values.values.compactMap {
                try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: $0)
            }.first?.configuration.stateKey
            precondition(failed.credentialLoadFailure != nil && failed.appliedConfiguration == nil)
            precondition(failed.stateEncryptionKey.isEmpty,
                         "failed bootstrap exposed an uncommitted queue key in memory")
            let failedQueue = ForwardingFixture.checkpointDirectory
                .appendingPathComponent("sms-forwarding/state.enc")
            let failedWitness = ForwardingFixture.checkpointDirectory
                .appendingPathComponent("sms-forwarding/state.enc.keyid")
            if writeFault == 1 || verifyFault == 1 {
                precondition(!FileManager.default.fileExists(atPath: failedQueue.path)
                             && !FileManager.default.fileExists(atPath: failedWitness.path),
                             "a stage failure published backend queue authority")
            }
            await failed.synchronizeAppliedConfiguration()
            await failed.save()
            precondition(ForwardingFixture.calls.count == calls,
                         "unconfirmed bootstrap reached the backend")
            precondition(ForwardingFixture.defaults.string(forKey: intentID) == "memory-only-legacy-intent"
                         && ForwardingFixture.defaults.string(forKey: intentSequence) == "41")
            ForwardingKeychain.replies = [:]
            ForwardingKeychain.failWriteAt = nil; ForwardingKeychain.failVerifyAt = nil
            let recovered = SMSForwardingStore()
            if writeFault == 1 && !afterWrite {
                // Rejected before any snapshot write, seed or witness: a new
                // bootstrap is safe because no durable K-prime was published.
                precondition(recovered.credentialLoadFailure == nil
                             && recovered.appliedConfiguration != nil)
            } else if !committedPointer {
                precondition(recovered.credentialLoadFailure != nil && recovered.appliedConfiguration == nil
                             && ForwardingFixture.calls.count == calls,
                             "an uncommitted stage must not be promoted or replaced")
                continue
            }
            precondition(recovered.credentialLoadFailure == nil && recovered.telegramBotToken == "legacy-token")
            precondition(recovered.configurationIntent.clientSequence == 42
                         && recovered.configurationIntent.clientRequestID != "memory-only-legacy-intent",
                         "a new key reused the legacy identity of a lost key")
            if committedPointer { precondition(recovered.stateEncryptionKey == attemptedKey) }
            let recoveredAccount = String(data: ForwardingKeychain.values[active]!, encoding: .utf8)!
            let durable = try! JSONDecoder().decode(SMSForwardingStoredConfiguration.self,
                from: ForwardingKeychain.values[recoveredAccount]!)
            precondition(durable.configuration.stateKey == recovered.stateEncryptionKey
                         && durable.intent == recovered.configurationIntent)
            await recovered.synchronizeAppliedConfiguration()
            precondition(ForwardingFixture.completedConfigurations.last?["state_key"] as? String
                         == durable.configuration.stateKey)
        }

        clearDefaults()
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        let queueDirectory = ForwardingFixture.checkpointDirectory.appendingPathComponent("sms-forwarding")
        let queue = queueDirectory.appendingPathComponent("state.enc")
        try! FileManager.default.createDirectory(at: queueDirectory, withIntermediateDirectories: false)
        let encrypted = Data("existing-encrypted-queue-must-survive".utf8)
        try! encrypted.write(to: queue)
        for values: [String: Data] in [[:], ["state.encryptionKey": Data("invalid-key".utf8)]] {
            ForwardingKeychain.reset(values)
            let calls = ForwardingFixture.calls.count
            let blocked = SMSForwardingStore()
            precondition(blocked.appliedConfiguration == nil && blocked.credentialLoadFailure != nil)
            precondition(blocked.errorMessage?.contains("原密钥") == true)
            await blocked.synchronizeAppliedConfiguration(); await blocked.save()
            precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls == 0)
            precondition(ForwardingFixture.calls.count == calls && (try! Data(contentsOf: queue)) == encrypted)
        }
        try! FileManager.default.removeItem(at: queueDirectory)

        // Rust holds this sibling lock before its first encrypted queue write.
        // A lost Keychain key must not be replaced while that owner is live.
        try! FileManager.default.createDirectory(at: queueDirectory, withIntermediateDirectories: false)
        let queueLock = queueDirectory.appendingPathComponent("state.enc.lock")
        let queueLockFD = open(queueLock.path, O_RDWR | O_CREAT | O_EXCL, 0o600)
        precondition(queueLockFD >= 0 && flock(queueLockFD, LOCK_EX | LOCK_NB) == 0)
        ForwardingKeychain.reset()
        let beforeBusyQueue = ForwardingFixture.calls.count
        let busyQueue = SMSForwardingStore()
        precondition(busyQueue.appliedConfiguration == nil && !busyQueue.credentialScopeLoaded
                     && busyQueue.errorMessage != nil,
                     "a live queue owner without state.enc allowed replacement key generation")
        precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls == 0
                     && ForwardingFixture.calls.count == beforeBusyQueue)
        _ = flock(queueLockFD, LOCK_UN)
        _ = close(queueLockFD)
        // A crashed owner left no ciphertext. The historical lock can be
        // reused after its flock is released, then K-prime seeds SWF1.
        ForwardingKeychain.reset()
        let staleQueueLock = SMSForwardingStore()
        precondition(staleQueueLock.appliedConfiguration != nil && staleQueueLock.credentialLoadFailure == nil
                     && ForwardingKeychain.randomCalls == 1)
        precondition((try! Data(contentsOf: queue)).starts(with: Data("SWF1".utf8)))
        try! FileManager.default.removeItem(at: queueDirectory)

        func storedWithSequence(_ sequence: UInt64) -> Data {
            try! JSONEncoder().encode(SMSForwardingStoredConfiguration(schemaVersion: 1,
                configuration: stored.configuration,
                intent: SMSForwardingConfigurationIntent(clientRequestID: "retained-\(sequence)", clientSequence: sequence)))
        }
        var crowded = committed
        crowded[firstAccount] = storedWithSequence(20)
        var sequenceAccounts: [UInt64: String] = [:]
        for sequence in 1...14 {
            let account = prefix + UUID().uuidString.lowercased()
            sequenceAccounts[UInt64(sequence)] = account
            crowded[account] = storedWithSequence(UInt64(sequence))
        }
        let orphan = prefix + UUID().uuidString.lowercased()
        crowded[orphan] = storedWithSequence(21)
        let foreignAccount = SMSForwardingCredentialScope.legacyService + ".root.v2.foreign|" + prefix + UUID().uuidString
        crowded[foreignAccount] = Data("foreign-untouched".utf8)
        crowded["legacy." + firstAccount] = committed[firstAccount]
        crowded[prefix + "not-a-uuid"] = Data("unrelated".utf8)
        ForwardingKeychain.reset(crowded)
        let restored = SMSForwardingStore()
        precondition(restored.credentialScopeLoaded && ForwardingKeychain.writes.filter { $0.hasPrefix("delete:") }.count == 8)
        precondition(ForwardingKeychain.values[orphan] == nil, "failed staging did not take GC priority")
        precondition(ForwardingKeychain.values[firstAccount] == crowded[firstAccount])
        precondition(ForwardingKeychain.values[sequenceAccounts[14]!] != nil
                     && ForwardingKeychain.values[sequenceAccounts[13]!] != nil)
        _ = SMSForwardingStore()
        precondition(ForwardingKeychain.accounts(service: ForwardingFixture.primaryService)
            .filter { $0.hasPrefix(prefix) && UUID(uuidString: String($0.dropFirst(prefix.count))) != nil }.count == 3)
        precondition(ForwardingKeychain.values[foreignAccount] == crowded[foreignAccount]
                     && ForwardingKeychain.values["legacy." + firstAccount] == committed[firstAccount])
        // A pointer/read failure during cleanup cannot delete any candidate.
        for fault in ["enumerate", "delete", "pointer"] {
            ForwardingKeychain.reset(crowded)
            if fault == "enumerate" { ForwardingKeychain.failEnumeration = true }
            if fault == "delete" { ForwardingKeychain.failDeletion = true }
            if fault == "pointer" {
                ForwardingKeychain.beforeEnumeration = {
                    ForwardingKeychain.replies[active] = .status(errSecAuthFailed)
                }
            }
            let healthy = SMSForwardingStore()
            precondition(healthy.credentialScopeLoaded && healthy.credentialLoadFailure == nil)
            precondition(ForwardingKeychain.values == crowded, "uncertain GC changed durable bytes")
        }
        // Persistent enumeration failure admits no new stage at all.
        ForwardingKeychain.reset(committed)
        let enumBlocked = SMSForwardingStore()
        ForwardingKeychain.failEnumeration = true
        let beforeEnum = ForwardingKeychain.values
        for _ in 0..<20 { await enumBlocked.save() }
        precondition(ForwardingKeychain.values == beforeEnum && enumBlocked.credentialLoadFailure == nil)
        // Persistent delete failure can consume only the finite 16-item budget.
        ForwardingKeychain.reset(committed)
        let deletionBlocked = SMSForwardingStore()
        ForwardingKeychain.failDeletion = true
        for _ in 0..<24 { await deletionBlocked.save() }
        let budgeted = ForwardingKeychain.values
        let snapshots = ForwardingKeychain.accounts(service: ForwardingFixture.primaryService)
            .filter { $0.hasPrefix(prefix) && UUID(uuidString: String($0.dropFirst(prefix.count))) != nil }
        precondition(snapshots.count == 16 && deletionBlocked.credentialLoadFailure == nil)
        let selected = String(data: budgeted[active]!, encoding: .utf8)!
        precondition(budgeted[selected] != nil)
        for _ in 0..<8 { await deletionBlocked.save() }
        precondition(ForwardingKeychain.values == budgeted, "persistent GC failure grew past the finite budget")
        ForwardingKeychain.failDeletion = false
        let recoveredGC = SMSForwardingStore()
        await recoveredGC.save()
        precondition(recoveredGC.errorMessage == nil)
        // Failure occurring only after a successful active commit is best effort.
        ForwardingKeychain.reset(committed)
        let postCommit = SMSForwardingStore()
        ForwardingKeychain.failEnumerationAt = ForwardingKeychain.enumerationCalls + 2
        await postCommit.save()
        precondition(postCommit.errorMessage == nil && postCommit.configurationIntent.clientSequence == firstIntent.clientSequence + 1)
        clearDefaults(); ForwardingKeychain.reset()
        print("Forwarding bootstrap/GC: key+intent durable, five commit faults, queue-key preservation, active+two recovery generations, at-most-eight deletes, namespace isolation and persistent-failure budget passed")
    }

    static func runtimeAliasRepliesStayFenced() async {
        let fm = FileManager.default
        let base = ForwardingFixture.checkpointDirectory.appendingPathComponent("runtime-alias-replies")
        try! fm.createDirectory(at: base, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let savedAccepted = ForwardingFixture.acceptedHandoff
        let savedConfiguration = ForwardingFixture.activeConfigurationID
        let savedEnabled = ForwardingFixture.activeTelegramEnabled
        let savedDispatches = ForwardingFixture.telegramTestCalls
        defer {
            ForwardingFixture.alternateRoot = nil
            ForwardingFixture.heldRoute = nil
            ForwardingFixture.acceptedHandoff = savedAccepted
            ForwardingFixture.activeConfigurationID = savedConfiguration
            ForwardingFixture.activeTelegramEnabled = savedEnabled
            ForwardingFixture.telegramTestCalls = savedDispatches
            try! fm.removeItem(at: base)
        }
        let routes = ["GET api/sms/forwarding/status", "POST api/sms/forwarding/queue-action",
                      "GET api/v2/sms/forwarding/handoff", "POST api/v2/sms/forwarding/handoff",
                      "POST api/sms/forwarding/configure", "POST api/sms/forwarding/test"]
        for (index, route) in routes.enumerated() {
            func directory(_ name: String) -> URL {
                let url = base.appendingPathComponent("\(index)-\(name)")
                try! fm.createDirectory(at: url, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
                return url
            }
            let rootA = directory("a"), rootB = directory("b")
            let alias = base.appendingPathComponent("\(index)-alias")
            try! fm.createSymbolicLink(at: alias, withDestinationURL: rootA)
            ForwardingFixture.alternateRoot = alias
            ForwardingFixture.acceptedHandoff = nil
            let checkpoint = SMSForwardingCheckpoint(directory: rootA)
            if route == "POST api/v2/sms/forwarding/handoff" {
                let original = SMSForwardingHandoffV1(schemaVersion: 1, handoffID: UUID().uuidString.lowercased(),
                    deviceSMS: DeviceSMSForwardingHandoff(acceptedNotificationIDs: [], wasEnabled: nil),
                    nwaitSMS: NwaitSMSForwardingHandoff(cursor: "original-a", seeded: true, wasEnabled: nil))
                _ = try! checkpoint.confirm(JSONEncoder().encode(original))
            }
            let store = SMSForwardingStore(checkpoint: checkpoint)
            let originalCheckpoint = try! checkpoint.loadConfirmed()
            let originalSecrets = ForwardingKeychain.values
            let originalPending = SMSForwardingPendingTest(request: SMSForwardingProviderRequest(
                clientRequestID: "held-alias-\(index)", provider: "telegram", expectedConfigurationID: "original-config"),
                configurationIntentID: store.configurationIntent.clientRequestID)
            store.pendingTests = ["telegram": originalPending]
            let pendingFile = ForwardingFixture.checkpointDirectory.appendingPathComponent(SMSForwardingTestCheckpoint.fileName)
            let originalPendingBytes = try? Data(contentsOf: pendingFile)
            ForwardingFixture.activeTelegramEnabled = true
            ForwardingFixture.activeConfigurationID = "original-config"
            ForwardingFixture.heldRoute = route
            let task = Task { @MainActor in
                switch route {
                case "GET api/sms/forwarding/status": await store.refreshStatus()
                case "POST api/sms/forwarding/queue-action":
                    await store.resolveTask(id: "original-task", provider: "telegram", action: "cancel",
                        expectedConfigurationID: "original-config", expectedAttempts: 1,
                        expectedState: "paused_result_unknown", expectedDestinationRevision: String(repeating: "a", count: 64))
                case "GET api/v2/sms/forwarding/handoff", "POST api/v2/sms/forwarding/handoff":
                    do {
                        _ = try await store.submitAdmissionHandoff(deviceAcceptedNotificationIDs: [],
                            deviceWasEnabled: nil, nwaitCursor: nil, nwaitSeeded: false, nwaitWasEnabled: nil)
                        fatalError("adopted handoff response after runtime root changed")
                    } catch {}
                case "POST api/sms/forwarding/configure": await store.synchronizeAppliedConfiguration()
                case "POST api/sms/forwarding/test":
                    await store.submitTest(originalPending, generation: store.uiSnapshotGeneration)
                default: fatalError("missing held route fixture")
                }
            }
            await until { ForwardingFixture.routeWaiters.count == 1 }
            let callsAtReply = ForwardingFixture.calls.count
            let handoffsAtReply = ForwardingFixture.handoffRequests.count
            try! fm.removeItem(at: alias)
            try! fm.createSymbolicLink(at: alias, withDestinationURL: rootB)
            ForwardingFixture.releaseRoute()
            await task.value
            precondition(store.status == nil && store.confirmedConfigurationID == nil
                         && !store.admissionHandoffSubmitted,
                         "adopted status/configuration/handoff after runtime root changed: \(route)")
            precondition(store.pendingTests["telegram"] == originalPending
                         && (try? Data(contentsOf: pendingFile)) == originalPendingBytes,
                         "cleared or migrated original pending-test checkpoint after alias change")
            precondition(try! checkpoint.loadConfirmed() == originalCheckpoint,
                         "held response changed original handoff checkpoint")
            precondition(try! fm.contentsOfDirectory(atPath: rootB.path).isEmpty,
                         "held response wrote a checkpoint into replacement root")
            precondition(ForwardingFixture.calls.count == callsAtReply
                         && ForwardingFixture.handoffRequests.count == handoffsAtReply,
                         "held response triggered a follow-up request after alias change")
            precondition(ForwardingKeychain.values == originalSecrets)
            ForwardingFixture.testReceipts.removeValue(forKey: originalPending.request.clientRequestID)
        }
        print("Runtime alias: zero requests after retarget; six held API responses retain status, handoff and pending-test fences")
    }

    static func credentialProcessMode(_ mode: String) async throws {
        let scope = try ForwardingFixture.credentialScope()
        if mode == "hold" {
            try scope.withLock {
                FileHandle.standardOutput.write(Data("held\n".utf8))
                precondition(FileHandle.standardInput.readData(ofLength: 1).count == 1)
            }
            return
        }
        let store = SMSForwardingStore()
        if mode == "busy" {
            precondition(!store.credentialScopeLoaded && store.appliedConfiguration == nil)
            precondition(store.credentialLoadFailure == nil && ForwardingKeychain.writes.isEmpty)
            FileHandle.standardOutput.write(Data("busy\n".utf8))
            return
        }
        if mode == "mutate-barriers" {
            ForwardingKeychain.barrierPhases = ["stage", "active", "delete"]
            await store.save()
            precondition(store.errorMessage == nil && ForwardingKeychain.barrierPhases.isEmpty)
        } else if mode == "mutate" {
            await store.save()
            precondition(store.errorMessage == nil)
        } else { precondition(mode == "bootstrap") }
        let value: [String: Any] = store.credentialScopeLoaded
            ? ["state": "ready", "key": store.stateEncryptionKey,
               "intent": store.configurationIntent.clientRequestID,
               "sequence": store.configurationIntent.clientSequence, "service": scope.service,
               "active": store.loadedActiveAccount!]
            : ["state": "busy"]
        FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: value))
    }

    static func credentialNamespacesAndRealLocks() async {
        let fm = FileManager.default
        let base = ForwardingFixture.checkpointDirectory.appendingPathComponent("credential-coordination")
        try! fm.createDirectory(at: base, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        func directory(_ name: String) -> URL {
            let value = base.appendingPathComponent(name)
            try! fm.createDirectory(at: value, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            return value
        }
        let rootA = directory("root-a"), rootB = directory("root-b")
        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingKeychain.reset(["telegram.botToken": Data("default-root-only-token".utf8)])
        ForwardingFixture.defaults.set(true, forKey: "switch.smsForwarding.telegram.enabled")
        let original = SMSForwardingStore()
        let originalKey = original.stateEncryptionKey
        let originalIntent = original.configurationIntent
        let originalValues = ForwardingKeychain.values
        ForwardingFixture.alternateRoot = rootA
        let a = SMSForwardingStore()
        precondition(a.credentialScopeLoaded && !a.telegramEnabled && a.telegramBotToken.isEmpty)
        precondition(a.stateEncryptionKey != originalKey && !a.credentialScope!.migratesLegacy)
        let keyA = a.stateEncryptionKey
        ForwardingFixture.alternateRoot = rootB
        let b = SMSForwardingStore()
        precondition(b.credentialScopeLoaded && b.stateEncryptionKey != keyA
                     && b.stateEncryptionKey != originalKey && !b.telegramEnabled)
        precondition(a.credentialScope!.service != b.credentialScope!.service)
        let alias = base.appendingPathComponent("root-a-alias")
        try! fm.createSymbolicLink(at: alias, withDestinationURL: rootA)
        ForwardingFixture.alternateRoot = alias
        let aliasStore = SMSForwardingStore()
        precondition(aliasStore.stateEncryptionKey == keyA && aliasStore.credentialScope!.service == a.credentialScope!.service)
        let aliasBytes = ForwardingKeychain.values
        let aliasWrites = ForwardingKeychain.writes.count
        let aliasCalls = ForwardingFixture.calls.count
        try! fm.removeItem(at: alias)
        try! fm.createSymbolicLink(at: alias, withDestinationURL: rootB)
        await aliasStore.save()
        await aliasStore.test("telegram")
        await aliasStore.synchronizeAppliedConfiguration()
        let aliasHandoffs = ForwardingFixture.handoffRequests.count
        await aliasStore.refreshStatus()
        await aliasStore.resolveTask(id: "original-task", provider: "telegram", action: "cancel",
            expectedConfigurationID: "original-config", expectedAttempts: 1,
            expectedState: "paused_result_unknown", expectedDestinationRevision: String(repeating: "a", count: 64))
        do {
            _ = try await aliasStore.submitAdmissionHandoff(deviceAcceptedNotificationIDs: [],
                deviceWasEnabled: nil, nwaitCursor: nil, nwaitSeeded: false, nwaitWasEnabled: nil)
            fatalError("retargeted runtime alias submitted a handoff")
        } catch {}
        do {
            _ = try await aliasStore.frozenAdmissionHandoff(deviceAcceptedNotificationIDs: [],
                deviceWasEnabled: nil, nwaitCursor: nil, nwaitSeeded: false, nwaitWasEnabled: nil)
            fatalError("retargeted runtime alias read or migrated a handoff")
        } catch {}
        precondition(ForwardingFixture.handoffRequests.count == aliasHandoffs)
        precondition(ForwardingKeychain.values == aliasBytes && ForwardingKeychain.writes.count == aliasWrites
                     && ForwardingFixture.calls.count == aliasCalls,
                     "retargeted runtime alias wrote secrets or routed an old key/test to another root")
        precondition(fm.fileExists(atPath: rootA.path) && fm.fileExists(atPath: rootB.path))
        // A foreign legacy sequence must not influence this v2 namespace.
        ForwardingFixture.defaults.set(String(UInt64.max), forKey: "switch.smsForwarding.configurationIntent.sequence")
        await a.save()
        precondition(a.errorMessage == nil && a.configurationIntent.clientSequence == 2)
        let scopedSequence = a.configurationIntent
        let scopeAccount = a.credentialScope!.service + "|" + a.loadedActiveAccount!
        let maximum = SMSForwardingStoredConfiguration(schemaVersion: 1,
            configuration: a.makeConfiguration(),
            intent: SMSForwardingConfigurationIntent(clientRequestID: scopedSequence.clientRequestID, clientSequence: UInt64.max))
        ForwardingKeychain.values[scopeAccount] = try! JSONEncoder().encode(maximum)
        ForwardingFixture.alternateRoot = rootA
        let exhausted = SMSForwardingStore()
        let beforeExhausted = ForwardingKeychain.values
        let writesBeforeExhausted = ForwardingKeychain.writes.count
        let callsBeforeExhausted = ForwardingFixture.calls.count
        await exhausted.save()
        precondition(exhausted.errorMessage?.contains("序号已用尽") == true)
        await exhausted.test("telegram")
        precondition(exhausted.telegramTestMessage?.contains("序号已用尽") == true
                     && ForwardingKeychain.values == beforeExhausted
                     && ForwardingKeychain.writes.count == writesBeforeExhausted
                     && ForwardingFixture.calls.count == callsBeforeExhausted,
                     "scoped sequence exhaustion crashed, wrote or uploaded")
        ForwardingFixture.defaults.removeObject(forKey: "switch.smsForwarding.configurationIntent.sequence")
        ForwardingFixture.alternateRoot = nil
        let defaultAgain = SMSForwardingStore()
        precondition(defaultAgain.stateEncryptionKey == originalKey && defaultAgain.configurationIntent == originalIntent)
        for (account, bytes) in originalValues { precondition(ForwardingKeychain.values[account] == bytes) }

        // A stale GUI must not overwrite a newer GUI's durable active pointer.
        let stale = SMSForwardingStore(), winner = SMSForwardingStore()
        winner.telegramChatID = "winner"
        await winner.save()
        let afterWinner = ForwardingKeychain.values
        let calls = ForwardingFixture.calls.count
        let writes = ForwardingKeychain.writes.count
        stale.telegramChatID = "stale"
        await stale.save()
        await stale.synchronizeAppliedConfiguration()
        precondition(ForwardingKeychain.values == afterWinner && ForwardingKeychain.writes.count == writes)
        precondition(ForwardingFixture.calls.count == calls && stale.errorMessage?.contains("版本已变化") == true)

        // Complete v1 legacy migration is one-way and only for the true default root.
        let oldActive = String(data: originalValues["configuration.active.v1"]!, encoding: .utf8)!
        let legacy = ["legacy.configuration.active.v1": Data(oldActive.utf8),
                      "legacy." + oldActive: originalValues[oldActive]!]
        ForwardingKeychain.reset(legacy)
        let migrated = SMSForwardingStore()
        precondition(migrated.credentialScopeLoaded && migrated.stateEncryptionKey == originalKey
                     && migrated.configurationIntent == originalIntent)
        for (account, bytes) in legacy { precondition(ForwardingKeychain.values[account] == bytes) }
        let migratedBytes = ForwardingKeychain.values
        _ = SMSForwardingStore()
        precondition(ForwardingKeychain.values == migratedBytes)
        let missingRoot = directory("root-with-queue")
        let queueDir = missingRoot.appendingPathComponent("sms-forwarding")
        try! fm.createDirectory(at: queueDir, withIntermediateDirectories: false)
        let queue = queueDir.appendingPathComponent("state.enc"), bytes = Data("foreign-root-original-queue".utf8)
        try! bytes.write(to: queue)
        ForwardingFixture.alternateRoot = missingRoot
        let blocked = SMSForwardingStore()
        precondition(!blocked.credentialScopeLoaded && blocked.appliedConfiguration == nil)
        precondition(ForwardingKeychain.values == migratedBytes && (try! Data(contentsOf: queue)) == bytes)
        ForwardingFixture.alternateRoot = nil

        // Exercise the actual OS lock in independent processes. Both roots use
        // the same explicitly isolated user-home namespace and mock secret IO.
        let sharedHome = directory("shared-user-home"), processRoot = directory("process-root")
        let secrets = directory("mock-secrets")
        func launch(_ mode: String, root: URL = processRoot) -> (Process, Pipe, Pipe) {
            let process = Process(), output = Pipe(), input = Pipe()
            process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
            var environment = ProcessInfo.processInfo.environment
            environment["SWITCH_FORWARDING_CREDENTIAL_MODE"] = mode
            environment["SWITCH_FORWARDING_CHECKPOINT_DIR"] = root.path
            environment["SWITCH_FORWARDING_CREDENTIAL_DEFAULT_ROOT"] = processRoot.path
            environment["SWITCH_FORWARDING_CREDENTIAL_HOME"] = sharedHome.path
            environment["SWITCH_FORWARDING_CREDENTIAL_SECRET_DIRECTORY"] = secrets.path
            process.environment = environment
            process.standardOutput = output; process.standardInput = input
            process.standardError = FileHandle.standardError
            try! process.run()
            return (process, output, input)
        }
        func finish(_ child: (Process, Pipe, Pipe)) -> Data {
            let bytes = child.1.fileHandleForReading.readDataToEndOfFile()
            child.0.waitUntilExit()
            precondition(child.0.terminationStatus == 0, "credential child failed")
            return bytes
        }
        ForwardingFixture.alternateLockHome = sharedHome
        let busySave = SMSForwardingStore()
        ForwardingFixture.alternateLockHome = nil
        precondition(busySave.credentialScopeLoaded)
        let holder = launch("hold")
        precondition(holder.1.fileHandleForReading.readData(ofLength: 5) == Data("held\n".utf8))
        let valuesBeforeBusy = ForwardingKeychain.values
        busySave.telegramChatID = "retry-after-lock-release"
        await busySave.save()
        precondition(busySave.credentialLoadFailure == nil && ForwardingKeychain.values == valuesBeforeBusy)
        precondition(busySave.errorMessage?.contains("稍后重试") == true)
        let contender = launch("busy", root: rootB)
        precondition(finish(contender) == Data("busy\n".utf8), "different DataRoot bypassed the fixed user lock")
        precondition(try! fm.contentsOfDirectory(atPath: secrets.path).isEmpty)
        holder.2.fileHandleForWriting.write(Data([1]))
        _ = finish(holder)
        await busySave.save()
        precondition(busySave.errorMessage == nil && busySave.credentialLoadFailure == nil,
                     "temporary lock contention permanently fenced a loaded instance")
        let first = launch("bootstrap"), second = launch("bootstrap")
        let results = [finish(first), finish(second)].map { try! JSONSerialization.jsonObject(with: $0) as! [String: Any] }
        let ready = results.filter { $0["state"] as? String == "ready" }
        precondition(!ready.isEmpty)
        precondition(Set(ready.map { $0["key"] as! String }).count == 1
                     && Set(ready.map { $0["intent"] as! String }).count == 1,
                     "concurrent bootstrap selected different keys or intents")
        let reopened = try! JSONSerialization.jsonObject(with: finish(launch("bootstrap"))) as! [String: Any]
        precondition(reopened["key"] as? String == ready[0]["key"] as? String
                     && reopened["intent"] as? String == ready[0]["intent"] as? String)
        precondition(try! fm.contentsOfDirectory(atPath: secrets.path).filter { !$0.hasSuffix(".account") }.count == 2,
                     "concurrent bootstrap committed more than one complete snapshot")
        let foreignRoot = directory("foreign-process-root")
        let foreign = try! JSONSerialization.jsonObject(with: finish(launch("bootstrap", root: foreignRoot))) as! [String: Any]
        precondition(foreign["key"] as? String != reopened["key"] as? String
                     && foreign["service"] as? String != reopened["service"] as? String)
        precondition(try! fm.contentsOfDirectory(atPath: secrets.path).filter { !$0.hasSuffix(".account") }.count == 4)

        _ = finish(launch("mutate")); _ = finish(launch("mutate"))
        let gcWriter = launch("mutate-barriers")
        for phase in ["stage", "active", "delete"] {
            precondition(gcWriter.1.fileHandleForReading.readData(ofLength: phase.utf8.count + 1)
                         == Data((phase + "\n").utf8), "GC process missed exact \(phase) barrier")
            let atBarrier = try! fm.contentsOfDirectory(at: secrets, includingPropertiesForKeys: nil)
                .reduce(into: [String: Data]()) { $0[$1.lastPathComponent] = try! Data(contentsOf: $1) }
            precondition(finish(launch("busy")) == Data("busy\n".utf8))
            let afterContender = try! fm.contentsOfDirectory(at: secrets, includingPropertiesForKeys: nil)
                .reduce(into: [String: Data]()) { $0[$1.lastPathComponent] = try! Data(contentsOf: $1) }
            precondition(afterContender == atBarrier, "second process staged while first held global lock")
            gcWriter.2.fileHandleForWriting.write(Data([1]))
        }
        let gcResult = try! JSONSerialization.jsonObject(with: finish(gcWriter)) as! [String: Any]
        let nextResult = try! JSONSerialization.jsonObject(with: finish(launch("mutate"))) as! [String: Any]
        precondition((nextResult["sequence"] as! NSNumber).uint64Value
                     == (gcResult["sequence"] as! NSNumber).uint64Value + 1)
        for receipt in [gcResult, nextResult] {
            let compound = (receipt["service"] as! String) + "|" + (receipt["active"] as! String)
            precondition(fm.fileExists(atPath: secrets.appendingPathComponent(SMSForwardingCredentialScope.digest(compound)).path),
                         "previous GC removed a successor or retained recovery snapshot")
        }
        let afterGC = try! JSONSerialization.jsonObject(with: finish(launch("bootstrap"))) as! [String: Any]
        precondition(afterGC["active"] as? String == nextResult["active"] as? String)

        // An unsafe lock path is rejected without changing it or touching any secret.
        for attack in ["directory-symlink", "directory-mode", "file-symlink", "file-hardlink", "file-mode"] {
            let home = directory(attack)
            let lockDirectory = home.appendingPathComponent(".switch-sms-forwarding-credentials-v2")
            let target = home.appendingPathComponent("untouched-target")
            try! Data("untouched".utf8).write(to: target)
            if attack == "directory-symlink" {
                try! fm.createSymbolicLink(at: lockDirectory, withDestinationURL: base)
            } else {
                try! fm.createDirectory(at: lockDirectory, withIntermediateDirectories: false,
                                        attributes: [.posixPermissions: attack == "directory-mode" ? 0o755 : 0o700])
                let file = lockDirectory.appendingPathComponent("configuration.lock")
                if attack == "file-symlink" { try! fm.createSymbolicLink(at: file, withDestinationURL: target) }
                else if attack == "file-hardlink" {
                    try! fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
                    try! fm.linkItem(at: target, to: file)
                } else if attack == "file-mode" {
                    try! Data().write(to: file)
                    try! fm.setAttributes([.posixPermissions: 0o644], ofItemAtPath: file.path)
                }
            }
            let scope = try! SMSForwardingCredentialScope(dataRoot: rootA, defaultRoot: processRoot, lockHome: home)
            var entered = false
            do { try scope.withLock { entered = true }; fatalError("unsafe lock path accepted") }
            catch {}
            precondition(!entered)
            precondition((try! Data(contentsOf: target)) == Data("untouched".utf8))
        }
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        ForwardingKeychain.reset(); ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        try! fm.removeItem(at: base)
        print("Credential coordination: default-only legacy migration, canonical aliases, independent roots, stale GUI fence, runtime-alias retarget and scoped sequence fences, real process stage/active/delete barriers, bounded GC and five unsafe lock paths passed")
    }

    static func recoveryProcessMode(_ mode: String) async {
        let environment = ProcessInfo.processInfo.environment
        precondition(environment["SWITCH_FORWARDING_CREDENTIAL_SECRET_DIRECTORY"] != nil,
                     "recovery tests require private file-backed mock Keychain")
        ForwardingKeychain.reset()
        let scope = try! ForwardingFixture.credentialScope()
        let activeName = scope.service + "|configuration.active.v1"
        let normal = mode == "baseline-fresh" || mode == "normal-reopen"
        if !normal { ForwardingKeychain.replies[activeName] = .status(errSecAuthFailed) }
        let store = SMSForwardingStore()
        if !normal {
            // A second process may see the actual owner's busy credential lock
            // before reaching the injected read failure; it must stay unloaded.
            precondition(!store.credentialScopeLoaded && store.appliedConfiguration == nil)
            ForwardingKeychain.replies.removeAll()
            precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls == 0)
        }
        ForwardingKeychain.barrierPhases = (environment["SWITCH_FORWARDING_RECOVERY_BARRIERS"] ?? "")
            .split(separator: ",").map(String.init)
        ForwardingKeychain.barrier("recovery-ready")
        switch environment["SWITCH_FORWARDING_RECOVERY_KEYCHAIN_FAULT"] {
        case "add-refused": ForwardingKeychain.failWriteAt = 1
        case "ack-lost": ForwardingKeychain.failWriteAt = 1; ForwardingKeychain.failAfterWrite = true
        case "readback-lost": ForwardingKeychain.failVerifyAt = 1
        case "enumeration-denied": ForwardingKeychain.failEnumeration = true
        default: break
        }
        var cancelled = false
        if mode == "inspect" || mode == "inspect-then-complete" {
            await store.inspectInitialSetupRecovery()
            precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls == 0
                         && store.appliedConfiguration == nil && store.stateEncryptionKey.isEmpty
                         && store.credentialLoadFailure != nil && ForwardingFixture.calls.isEmpty)
            if mode == "inspect-then-complete" {
                ForwardingKeychain.barrier("recovery-inspected")
                await store.completeInterruptedInitialSetup()
            }
        } else if mode == "cancel-after-active" {
            let task = Task { await store.completeInterruptedInitialSetup() }
            let deadline = DispatchTime.now().uptimeNanoseconds + 5_000_000_000
            while !ForwardingKeychain.didReach("active") {
                precondition(DispatchTime.now().uptimeNanoseconds < deadline, "worker did not reach actual pointer add")
                try? await Task.sleep(nanoseconds: 1_000_000)
            }
            // A second explicit UI click returns while the true first owner is
            // still blocked in SecItemAdd; it cannot enqueue another commit.
            await store.completeInterruptedInitialSetup()
            precondition(store.isCredentialRecoveryInProgress && !store.credentialScopeLoaded)
            task.cancel(); cancelled = task.isCancelled
            FileHandle.standardOutput.write(Data("observer-cancelled\n".utf8))
            await task.value
        } else if !normal {
            precondition(mode == "complete" || mode == "complete-twice")
            await store.completeInterruptedInitialSetup()
            if mode == "complete-twice" {
                let writes = ForwardingKeychain.writes.count
                await store.inspectInitialSetupRecovery()
                await store.completeInterruptedInitialSetup()
                precondition(ForwardingKeychain.writes.count == writes,
                             "unknown attempt gained authority from later inspection")
            }
        }
        precondition(!store.isCredentialRecoveryInProgress && ForwardingFixture.calls.isEmpty)
        var preserved = false
        if let account = ForwardingKeychain.value(activeName).flatMap({ String(data: $0, encoding: .utf8) }),
           let data = ForwardingKeychain.value(scope.service + "|" + account),
           let snapshot = try? JSONDecoder().decode(SMSForwardingStoredConfiguration.self, from: data),
           let actual = store.appliedConfiguration {
            let encoder = JSONEncoder(); encoder.outputFormatting = .sortedKeys
            preserved = try! encoder.encode(snapshot.configuration) == encoder.encode(actual)
        }
        let output: [String: Any] = [
            "completed": store.credentialScopeLoaded && store.credentialLoadFailure == nil,
            "applied": store.appliedConfiguration != nil, "key_loaded": !store.stateEncryptionKey.isEmpty,
            "can_attempt": store.canAttemptInitialSetupCompletion,
            "unconfirmed": store.initialSetupRecoveryUnconfirmed,
            "random_calls": ForwardingKeychain.randomCalls, "keychain_attempts": ForwardingKeychain.writes.count,
            "backend_calls": ForwardingFixture.calls.count, "cancelled": cancelled,
            "pointer_present": ForwardingKeychain.value(activeName) != nil,
            "settings_preserved": preserved, "telegram_enabled": store.telegramEnabled,
            "bark_enabled": store.barkEnabled,
            "form_credentials_blank": store.telegramBotToken.isEmpty && store.telegramChatID.isEmpty
                && store.barkDeviceKey.isEmpty && store.barkEncryptionKey.isEmpty,
            "legacy_unsupported": store.credentialRecoveryMessage?.contains("旧版迁移") == true,
            "busy": store.credentialRecoveryMessage?.contains("稍后重试") == true]
        FileHandle.standardOutput.write(try! JSONSerialization.data(withJSONObject: output, options: .sortedKeys))
        FileHandle.standardOutput.write(Data("\n".utf8))
    }

    static func main() async {
        if let mode = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_RECOVERY_MODE"] {
            await recoveryProcessMode(mode); return
        }
        if let mode = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_DIAGNOSTIC_MODE"] {
            ForwardingKeychain.reset()
            let scope = try! ForwardingFixture.credentialScope()
            // Construct a permanently fenced instance without startup bootstrap,
            // GC, active selection or real Security. Diagnosis is independent of
            // that initialization failure and cannot clear its fence.
            ForwardingKeychain.replies[scope.service + "|configuration.active.v1"] = .status(errSecAuthFailed)
            let store = SMSForwardingStore()
            precondition(store.credentialLoadFailure != nil && store.appliedConfiguration == nil)
            ForwardingKeychain.replies.removeAll()
            if mode == "deny-read" { ForwardingKeychain.failEnumeration = true }
            if mode == "pause" {
                ForwardingKeychain.barrierPhases = (ProcessInfo.processInfo.environment[
                    "SWITCH_FORWARDING_DIAGNOSTIC_BARRIER"] ?? "diagnostic-ready")
                    .split(separator: ",").map(String.init)
            }
            let before = store.credentialLoadFailure
            ForwardingKeychain.barrier("diagnostic-ready")
            let result = store.diagnoseCredentialRecovery()
            precondition(ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls == 0
                         && ForwardingFixture.calls.isEmpty && store.appliedConfiguration == nil
                         && store.stateEncryptionKey.isEmpty && store.credentialLoadFailure == before)
            let output: [String: Any] = ["state": result.state.rawValue, "witness": result.witness.rawValue,
                "snapshots": result.snapshotCount, "active": result.activePointerPresent]
            FileHandle.standardOutput.write(try! JSONSerialization.data(withJSONObject: output, options: .sortedKeys))
            FileHandle.standardOutput.write(Data("\n".utf8))
            return
        }
        if let mode = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_KEY_CROSS_MODE"] {
            ForwardingKeychain.reset()
            switch ProcessInfo.processInfo.environment["SWITCH_FORWARDING_KEY_CROSS_FAULT"] {
            case "stage-write": ForwardingKeychain.failWriteAt = 1
            case "stage-readback": ForwardingKeychain.failVerifyAt = 1
            case "active-write": ForwardingKeychain.failWriteAt = 2
            case "active-readback": ForwardingKeychain.failVerifyAt = 2
            default: break
            }
            if mode == "swift-bootstrap-pause" {
                ForwardingKeychain.barrierPhases = [ProcessInfo.processInfo.environment[
                    "SWITCH_FORWARDING_KEY_CROSS_BARRIER"] ?? "stage"]
                let store = SMSForwardingStore()
                precondition(store.credentialLoadFailure == nil && store.appliedConfiguration != nil)
                FileHandle.standardOutput.write(Data("committed\n".utf8))
            } else if mode == "swift-existing" {
                let store = SMSForwardingStore()
                precondition(store.credentialLoadFailure == nil && store.appliedConfiguration != nil
                             && ForwardingKeychain.randomCalls == 0 && ForwardingKeychain.writes.isEmpty)
                FileHandle.standardOutput.write(Data("existing\n".utf8))
            } else if mode == "swift-fail" {
                let store = SMSForwardingStore()
                precondition(store.credentialLoadFailure != nil && store.appliedConfiguration == nil
                             && store.stateEncryptionKey.isEmpty && ForwardingFixture.calls.isEmpty)
                FileHandle.standardOutput.write(Data("failed\n".utf8))
            } else if mode == "swift-busy" {
                let store = SMSForwardingStore()
                precondition(!store.credentialScopeLoaded && store.appliedConfiguration == nil
                             && ForwardingKeychain.randomCalls == 0 && ForwardingKeychain.writes.isEmpty)
                FileHandle.standardOutput.write(Data("busy\n".utf8))
            } else {
                precondition(mode == "swift-reject")
                let store = SMSForwardingStore()
                precondition(store.credentialLoadFailure != nil && store.appliedConfiguration == nil
                             && ForwardingKeychain.randomCalls == 0 && ForwardingKeychain.writes.isEmpty)
                FileHandle.standardOutput.write(Data("rejected\n".utf8))
            }
            return
        }
        if ProcessInfo.processInfo.environment["SWITCH_FORWARDING_KEY_PROTOCOL_ONLY"] == "1" {
            await keyProtocolCrashAndLegacyCases()
            return
        }
        if let mode = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_CREDENTIAL_MODE"] {
            do { try await credentialProcessMode(mode) }
            catch { fatalError("credential process failed: \(error)") }
            return
        }
        #if SWITCH_CHECKPOINT_TESTS
        if ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_COMMIT_FAILURE_ONLY"] == "1" {
            do { try await committedSyncFailureKeepsAdmissionPaused() }
            catch { fatalError("commit failure recovery failed: \(error)") }
            return
        }
        #endif
        if ForwardingFixture.persistenceFailureOnly {
            await persistenceFailureKeepsAdmissionPaused()
            return
        }
        if let mode = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_MODE"] {
            do { try await independentMode(mode) }
            catch { fatalError("independent handoff mode failed: \(error)") }
            return
        }
        defer { ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain) }
        await credentialReadFailuresStayFenced()
        await configurationCommitFailuresNeverMixSnapshots()
        await bootstrapAndBoundedReclamationStayDurable()
        await credentialNamespacesAndRealLocks()
        await runtimeAliasRepliesStayFenced()
        ForwardingFixture.defaults.set(true, forKey: "switch.smsForwarding.telegram.enabled")

        // The newer test snapshot completes before the older save. Rust would
        // reject that late lower sequence; the stale Swift continuation must
        // neither read shared error state nor send to an unbound target.
        ForwardingFixture.holdConfigure = true
        ForwardingFixture.activeTelegramEnabled = false
        let raceStore = SMSForwardingStore()
        let saveTask = Task { await raceStore.save() }
        await until { ForwardingFixture.waiters.count == 1 }
        let testTask = Task { await raceStore.test("telegram") }
        await until { ForwardingFixture.waiters.count == 2 }
        precondition(ForwardingFixture.telegramTestCalls == 0,
                     "Telegram test ran before configuration synchronization completed")
        ForwardingFixture.resolve(1)
        await until { ForwardingFixture.telegramTestCalls == 1 }
        precondition(ForwardingFixture.testBodies.last?["expected_configuration_id"] as? String
                     == ForwardingFixture.activeConfigurationID,
                     "Telegram test did not bind the exact acknowledged configuration")
        ForwardingFixture.reject(0)
        ForwardingFixture.holdConfigure = false
        await saveTask.value
        await testTask.value
        precondition(ForwardingFixture.telegramTestCalls == 1
                     && raceStore.telegramTestMessage == "测试通知已发送"
                     && raceStore.errorMessage == nil,
                     "stale save completion overwrote the newer test outcome")

        let durableID = raceStore.configurationIntent.clientRequestID
        let durableSequence = raceStore.configurationIntent.clientSequence
        let restartedStore = SMSForwardingStore()
        precondition(restartedStore.configurationIntent.clientRequestID == durableID
                     && restartedStore.configurationIntent.clientSequence == durableSequence,
                     "GUI restart did not reuse the durable configuration intent identity")
        await durableTestReceipts()
        failurePresentationIsActionableAndRedacted()
        let revisionStore = SMSForwardingStore()
        let callsBeforeRevisionMismatch = ForwardingFixture.calls.count
        ForwardingFixture.omitConfigurationID = true
        await revisionStore.save()
        ForwardingFixture.omitConfigurationID = false
        precondition(revisionStore.errorMessage == "本机转发配置已发生变化，请刷新后重试。",
                     "Swift accepted a response that did not match Rust's active desired revision")
        precondition(ForwardingFixture.calls.count == callsBeforeRevisionMismatch + 2,
                     "a revision mismatch triggered a hidden Swift configure retry")
        try! FileManager.default.removeItem(at: ForwardingFixture.checkpointDirectory.appendingPathComponent(SMSForwardingCheckpoint.fileName))
        ForwardingFixture.defaults.removePersistentDomain(forName: ForwardingFixture.domain)
        ForwardingFixture.defaults.set(true, forKey: "switch.smsForwarding.telegram.enabled")
        ForwardingFixture.calls.removeAll()
        ForwardingFixture.handoffRequests.removeAll()
        ForwardingFixture.acceptedHandoff = nil
        ForwardingFixture.waiters.removeAll()
        ForwardingFixture.configureBodies.removeAll()
        ForwardingFixture.testBodies.removeAll()
        ForwardingFixture.holdConfigure = false
        ForwardingFixture.activeConfigurationID = nil
        ForwardingFixture.activeTelegramEnabled = false
        ForwardingFixture.telegramTestCalls = 0

        let store = SMSForwardingStore()
        ForwardingFixture.failHandoffOnce = true
        let acceptedID = String(repeating: "a", count: 64)
        do {
            _ = try await store.submitAdmissionHandoff(
                deviceAcceptedNotificationIDs: [acceptedID], deviceWasEnabled: true,
                nwaitCursor: "opaque-cursor", nwaitSeeded: true, nwaitWasEnabled: false)
            fatalError("failed handoff unexpectedly returned an acknowledgement")
        } catch let failure as APIError {
            precondition(failure.statusCode == 503,
                         "first handoff did not reach the expected mock API failure")
        } catch {
            fatalError("first handoff failed before the expected mock API 503")
        }
        precondition(ForwardingFixture.handoffRequests.count == 1
                     && !ForwardingFixture.failHandoffOnce,
                     "first handoff did not consume exactly one mock API failure")
        let checkpoint = SMSForwardingCheckpoint(directory: ForwardingFixture.checkpointDirectory)
        let frozenHandoff = try! checkpoint.loadConfirmed()!
        let originalSnapshot = try! JSONDecoder().decode(SMSForwardingHandoffV1.self, from: frozenHandoff)
        let handoffAck = try! await store.submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: [String(repeating: "b", count: 64)], deviceWasEnabled: false,
            nwaitCursor: "changed-cursor", nwaitSeeded: false, nwaitWasEnabled: true)
        let firstHandoff = try! JSONDecoder().decode(
            SMSForwardingHandoffV1.self,
            from: JSONSerialization.data(withJSONObject: ForwardingFixture.handoffRequests[0]))
        let secondHandoff = try! JSONDecoder().decode(
            SMSForwardingHandoffV1.self,
            from: JSONSerialization.data(withJSONObject: ForwardingFixture.handoffRequests[1]))
        precondition(handoffAck.handoffID == originalSnapshot.handoffID && handoffAck.accepted,
                     "handoff retry did not use the frozen durable snapshot ID")
        precondition(ForwardingFixture.handoffRequests.count == 2
                     && firstHandoff == originalSnapshot && secondHandoff == originalSnapshot,
                     "handoff retry changed any field in the frozen source snapshot")
        if let fixturePath = ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH"],
           let buildPath = ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"] {
            let target = URL(fileURLWithPath: fixturePath).standardizedFileURL
            let buildRoot = URL(fileURLWithPath: buildPath).resolvingSymlinksInPath().standardizedFileURL
            let parent = target.deletingLastPathComponent().resolvingSymlinksInPath().standardizedFileURL
            precondition(parent.path.hasPrefix(buildRoot.path + "/"),
                         "cross-language fixture must stay under Switch/build")
            precondition(!FileManager.default.fileExists(atPath: target.path),
                         "cross-language fixture must not overwrite an existing file")
            try! frozenHandoff.write(to: target, options: .atomic)
        }
        ForwardingFixture.defaults.removeObject(forKey: "switch.smsForwarding.rustHandoffSnapshotV1")
        try! FileManager.default.removeItem(at: ForwardingFixture.checkpointDirectory.appendingPathComponent(SMSForwardingCheckpoint.fileName))
        let recoveredStore = SMSForwardingStore(checkpoint: checkpoint)
        let recoveredAck = try! await recoveredStore.submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil,
            nwaitCursor: nil, nwaitSeeded: false, nwaitWasEnabled: nil)
        let recoveredHandoff = try! JSONDecoder().decode(
            SMSForwardingHandoffV1.self,
            from: JSONSerialization.data(withJSONObject: ForwardingFixture.handoffRequests[2]))
        precondition(recoveredAck.handoffID == originalSnapshot.handoffID
                     && recoveredHandoff == originalSnapshot
                     && ForwardingFixture.calls.filter { $0 == "GET api/v2/sms/forwarding/handoff" }.count == 2,
                     "client did not recover the already durable snapshot from Rust after local preference loss")
        precondition(!ForwardingFixture.calls.contains("POST api/sms/forwarding/enqueue"),
                     "Swift store must not own an SMS forwarding producer")
        let old = try! JSONDecoder().decode(SMSForwardingStatus.self, from: ForwardingFixture.statusData())
        precondition(old.tasks == nil && old.telegram == nil, "legacy status response lost compatibility")
        let unknown = try! JSONDecoder().decode(SMSForwardingChannelStatus.self, from: Data("{\"state\":\"paused_result_unknown\",\"attempts\":1,\"revision\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"error_code\":\"bark_response_invalid\"}".utf8))
        precondition(unknown.canRetry && unknown.requiresRetryConfirmation && unknown.canCancel,
                     "uncertain submission must allow explicit retry/cancel with confirmation")
        precondition(unknown.label.contains("未知") && unknown.label.contains("暂停"),
                     "uncertain submission presented as success or normal automatic retry")
        let accepted = try! JSONDecoder().decode(SMSForwardingChannelStatus.self, from: Data("{\"state\":\"delivered\",\"attempts\":1,\"revision\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}".utf8))
        precondition(!accepted.canRetry && !accepted.requiresRetryConfirmation && accepted.label == "服务已接收",
                     "service acknowledgement must not promise phone delivery")
        let partial = try! JSONDecoder().decode(SMSForwardingChannelStatus.self, from: Data("{\"state\":\"retry_wait\",\"attempts\":1,\"revision\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"confirmed_parts\":1,\"total_parts\":3}".utf8))
        precondition(partial.confirmedParts == 1 && partial.totalParts == 3 && !partial.requiresRetryConfirmation,
                     "confirmed-part diagnostics did not decode independently of full service acceptance")
        let mismatch = try! JSONDecoder().decode(SMSForwardingChannelStatus.self, from: Data("{\"state\":\"paused_payload_mismatch\",\"attempts\":1,\"revision\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}".utf8))
        precondition(!mismatch.canRetry && mismatch.canCancel && mismatch.label.contains("不匹配"),
                     "incompatible fixed payload must pause rather than suggest blind retransmission")
        // An existing production file wins over conflicting or corrupt legacy
        // preferences. No backend GET or replacement snapshot is permitted.
        ForwardingFixture.defaults.set(Data("invalid ignored legacy".utf8), forKey: "switch.smsForwarding.rustHandoffSnapshotV1")
        let beforePriority = ForwardingFixture.calls.count
        let priorityBytes = try! checkpoint.loadConfirmed()!
        let priority = try! await SMSForwardingStore(checkpoint: checkpoint).submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
            nwaitSeeded: false, nwaitWasEnabled: nil)
        precondition(priority.handoffID == originalSnapshot.handoffID)
        precondition(ForwardingFixture.calls.count == beforePriority)
        precondition(try! checkpoint.loadConfirmed() == priorityBytes)

        // A corrupt existing file does not fall back to a valid legacy key.
        let target = ForwardingFixture.checkpointDirectory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
        try! FileManager.default.removeItem(at: target)
        ForwardingFixture.defaults.set(frozenHandoff, forKey: "switch.smsForwarding.rustHandoffSnapshotV1")
        let corrupt = Data("corrupt durable checkpoint".utf8)
        try! corrupt.write(to: target)
        precondition(chmod(target.path, 0o600) == 0)
        let beforeCorrupt = ForwardingFixture.handoffRequests.count
        do {
            _ = try await SMSForwardingStore(checkpoint: checkpoint).submitAdmissionHandoff(
                deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
                nwaitSeeded: false, nwaitWasEnabled: nil)
            fatalError("corrupt file fell back to valid legacy preferences")
        } catch let failure as SMSForwardingHandoffError {
            guard case .invalidFrozenSnapshot = failure else { fatalError("corrupt file error changed") }
        } catch { fatalError("unexpected corrupt file error") }
        precondition(ForwardingFixture.handoffRequests.count == beforeCorrupt)
        precondition(ForwardingFixture.calls.count == beforePriority)
        precondition(try! Data(contentsOf: target) == corrupt)

        // Legacy migration preserves the original ID and preference bytes.
        try! FileManager.default.removeItem(at: ForwardingFixture.checkpointDirectory.appendingPathComponent(SMSForwardingCheckpoint.fileName))
        ForwardingFixture.defaults.set(frozenHandoff, forKey: "switch.smsForwarding.rustHandoffSnapshotV1")
        let legacyCalls = ForwardingFixture.calls.count
        let migrated = SMSForwardingStore(checkpoint: checkpoint)
        let migratedAck = try! await migrated.submitAdmissionHandoff(
            deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
            nwaitSeeded: false, nwaitWasEnabled: nil)
        precondition(migratedAck.handoffID == originalSnapshot.handoffID)
        precondition(ForwardingFixture.calls.count == legacyCalls)
        precondition(ForwardingFixture.defaults.data(forKey: "switch.smsForwarding.rustHandoffSnapshotV1") == frozenHandoff)
        precondition(try! checkpoint.loadConfirmed() == frozenHandoff)

        try! FileManager.default.removeItem(at: ForwardingFixture.checkpointDirectory.appendingPathComponent(SMSForwardingCheckpoint.fileName))
        ForwardingFixture.defaults.set(Data("invalid legacy JSON".utf8), forKey: "switch.smsForwarding.rustHandoffSnapshotV1")
        let beforeInvalid = ForwardingFixture.handoffRequests.count
        do {
            _ = try await SMSForwardingStore(checkpoint: checkpoint).submitAdmissionHandoff(
                deviceAcceptedNotificationIDs: [], deviceWasEnabled: nil, nwaitCursor: nil,
                nwaitSeeded: false, nwaitWasEnabled: nil)
            fatalError("invalid legacy checkpoint silently generated a new ID")
        } catch let failure as SMSForwardingHandoffError {
            guard case .invalidFrozenSnapshot = failure else { fatalError("invalid legacy error changed") }
        } catch { fatalError("unexpected invalid legacy error") }
        precondition(ForwardingFixture.handoffRequests.count == beforeInvalid)
        precondition(ForwardingFixture.calls.count == legacyCalls)
        await statusReadsStayFenced()
        print("Forwarding client: production file freeze/retry/recovery/legacy migration and configuration races passed")
    }

    static func keyProtocolCrashAndLegacyCases() async {
        let crashRoot = ForwardingFixture.protocolRoot("crash")
        ForwardingFixture.alternateRoot = crashRoot
        ForwardingKeychain.reset()
        ForwardingKeychain.failWriteAt = 1
        let failed = SMSForwardingStore()
        precondition(failed.credentialLoadFailure != nil && failed.appliedConfiguration == nil)
        let witness = crashRoot.appendingPathComponent("sms-forwarding/state.enc.keyid")
        precondition((try! Data(contentsOf: witness)).count == 40)
        let queue = crashRoot.appendingPathComponent("sms-forwarding/state.enc")
        let originalQueue = try! Data(contentsOf: queue)
        precondition(originalQueue.count == 56 && originalQueue.starts(with: Data("SWF1".utf8)))
        ForwardingKeychain.reset()
        let restart = SMSForwardingStore()
        precondition(restart.credentialLoadFailure != nil && restart.appliedConfiguration == nil
                     && ForwardingKeychain.randomCalls == 0 && ForwardingKeychain.writes.isEmpty,
                     "crash between durable witness and Keychain publication must fence restart")
        precondition((try! Data(contentsOf: queue)) == originalQueue)

        let activeRoot = ForwardingFixture.protocolRoot("active")
        ForwardingFixture.alternateRoot = activeRoot
        ForwardingKeychain.reset()
        let first = SMSForwardingStore()
        precondition(first.credentialLoadFailure == nil)
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        ForwardingKeychain.randomCalls = 0
        let activeWithoutWitness = SMSForwardingStore()
        precondition(activeWithoutWitness.credentialLoadFailure == nil
                     && activeWithoutWitness.stateEncryptionKey == first.stateEncryptionKey
                     && ForwardingKeychain.randomCalls == 0,
                     "a committed Keychain snapshot without a queue must retain its key")

        ForwardingFixture.alternateRoot = nil
        ForwardingFixture.discardEmptyQueueAuthorityForTest()
        let legacyKey = Data(repeating: 3, count: 32).base64EncodedString()
        ForwardingKeychain.reset(["state.encryptionKey": Data(legacyKey.utf8)])
        let directory = ForwardingFixture.checkpointDirectory.appendingPathComponent("sms-forwarding")
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let lock = directory.appendingPathComponent("state.enc.lock")
        let lockFD = open(lock.path, O_RDWR | O_CREAT | O_EXCL, 0o600)
        precondition(lockFD >= 0)
        _ = close(lockFD)
        let recovered = SMSForwardingStore()
        precondition(recovered.credentialLoadFailure == nil && recovered.stateEncryptionKey == legacyKey
                     && ForwardingKeychain.randomCalls == 0,
                     "legacy K with empty Rust lock must migrate without key rotation")
        for (name, hardlink) in [("state.enc.bootstrap.lock", false), ("state.enc.keyid", true)] {
            let hostileRoot = ForwardingFixture.protocolRoot("hostile")
            ForwardingFixture.alternateRoot = hostileRoot
            ForwardingKeychain.reset()
            let queue = hostileRoot.appendingPathComponent("sms-forwarding")
            try! FileManager.default.createDirectory(at: queue, withIntermediateDirectories: false,
                                                     attributes: [.posixPermissions: 0o700])
            let target = hostileRoot.appendingPathComponent("outside")
            try! Data("untouched".utf8).write(to: target)
            try! FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
            let entry = queue.appendingPathComponent(name)
            if hardlink { try! FileManager.default.linkItem(at: target, to: entry) }
            else { try! FileManager.default.createSymbolicLink(at: entry, withDestinationURL: target) }
            let rejected = SMSForwardingStore()
            precondition(rejected.credentialLoadFailure != nil && rejected.appliedConfiguration == nil
                         && ForwardingKeychain.randomCalls == 0 && ForwardingKeychain.writes.isEmpty)
            precondition((try! Data(contentsOf: target)) == Data("untouched".utf8))
        }
        print("Queue key protocol: witness-before-Keychain crash, active snapshot without queue, and legacy empty lock passed")
    }
}
