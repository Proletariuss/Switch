import Foundation

struct FixtureFailure: Error, CustomStringConvertible {
    let description: String
}

@MainActor
func expect(_ value: @autoclosure () -> Bool, _ message: String) throws {
    if !value() { throw FixtureFailure(description: message) }
}

@MainActor
func record(_ request: DeviceSMSIntentRequest, state: DeviceSMSSendState = .prepared,
            canSubmit: Bool = true, archived: Bool = false) -> DeviceSMSIntent {
    DeviceSMSIntent(clientRequestID: request.clientRequestID, phone: request.phone,
                    message: request.message, device: request.device,
                    subscriptionID: request.expectedSubscriptionID, state: state,
                    segments: state == .accepted ? 1 : 0, archiveConfirmed: archived,
                    canSubmit: canSubmit, createdAtMS: 100, updatedAtMS: 101)
}

@MainActor
final class FixtureGate<Value> {
    private var continuation: CheckedContinuation<Value, Never>?
    private(set) var entered = false
    func wait() async -> Value {
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            entered = true
        }
    }
    func release(_ value: Value) {
        continuation?.resume(returning: value)
        continuation = nil
    }
}

@MainActor
func waitUntil(_ condition: @escaping @MainActor () -> Bool) async throws {
    for _ in 0..<10_000 {
        if condition() { return }
        await Task.yield()
    }
    throw FixtureFailure(description: "fixture did not reach suspended operation")
}

@MainActor
final class FixtureAPI: DeviceSMSSendAPI {
    var rows: [String: DeviceSMSIntent] = [:]
    var prepareCalls: [DeviceSMSIntentRequest] = []
    var submitCalls: [DeviceSMSIntentRequest] = []
    var getCalls: [String] = []
    var listCalls: [String?] = []
    var prepareHook: ((DeviceSMSIntentRequest) async throws -> DeviceSMSIntent)?
    var submitHook: ((DeviceSMSIntentRequest) async throws -> Void)?
    var getHook: ((String) async throws -> DeviceSMSIntent)?
    var listHook: ((String?) async throws -> DeviceSMSIntentPage)?

    func list(cursor: String?) async throws -> DeviceSMSIntentPage {
        listCalls.append(cursor)
        if let listHook { return try await listHook(cursor) }
        let sorted = rows.values.sorted { $0.id < $1.id }
        let start = cursor.flatMap(Int.init) ?? 0
        let end = min(start + 100, sorted.count)
        return DeviceSMSIntentPage(intents: Array(sorted[start..<end]),
                                   nextCursor: end < sorted.count ? String(end) : nil)
    }
    func get(id: String) async throws -> DeviceSMSIntent {
        getCalls.append(id)
        if let getHook { return try await getHook(id) }
        guard let row = rows[id] else { throw URLError(.resourceUnavailable) }
        return row
    }
    func prepare(_ request: DeviceSMSIntentRequest) async throws -> DeviceSMSIntent {
        prepareCalls.append(request)
        if let prepareHook { return try await prepareHook(request) }
        if let row = rows[request.clientRequestID] { return row }
        let row = record(request)
        rows[row.id] = row
        return row
    }
    func submit(_ request: DeviceSMSIntentRequest) async throws {
        submitCalls.append(request)
        if let submitHook { return try await submitHook(request) }
        rows[request.clientRequestID] = record(request, state: .accepted, canSubmit: false, archived: true)
    }
}

@MainActor
@main
struct DeviceSMSSendTests {
    static let phone = "+10000000000"
    static let message = "fixture-message"
    static let subscription = "sub1_" + String(repeating: "1", count: 64)
    static func request(_ id: String) -> DeviceSMSIntentRequest {
        DeviceSMSIntentRequest(clientRequestID: id, phone: phone, message: message,
                               device: "reader", expectedSubscriptionID: subscription)
    }
    static func makeStore(_ api: FixtureAPI) -> DeviceSMSSendStore {
        let store = DeviceSMSSendStore(api: api)
        store.setAvailable(true)
        return store
    }
    static func send(_ store: DeviceSMSSendStore, newRequest: Bool = false) async throws -> DeviceSMSIntent {
        try await store.send(phone: phone, message: message, device: "reader", subscriptionID: subscription, newRequest: newRequest)
    }
    static func pending(_ operation: () async throws -> DeviceSMSIntent) async throws {
        do {
            _ = try await operation()
            throw FixtureFailure(description: "expected unresolved result")
        } catch is DeviceSMSPendingError {}
    }

    static func testPrepareFailureRetainsRequestID() async throws {
        let api = FixtureAPI()
        api.prepareHook = { _ in throw URLError(.cannotConnectToHost) }
        let store = makeStore(api)
        try await pending { try await send(store) }
        try expect(api.submitCalls.isEmpty, "failed prepare authorized submit")
        let oldID = api.prepareCalls[0].clientRequestID
        api.prepareHook = nil
        let result = try await send(store)
        try expect(result.state == .accepted && api.prepareCalls.count == 2, "explicit retry did not finish")
        try expect(api.prepareCalls[1].clientRequestID == oldID && api.submitCalls.count == 1, "prepare failure silently created a second ID")
    }

    static func testPrepareLostACKNeedsExplicitResume() async throws {
        let api = FixtureAPI()
        api.prepareHook = { request in
            api.rows[request.clientRequestID] = record(request)
            throw URLError(.networkConnectionLost)
        }
        let store = makeStore(api)
        try await pending { try await send(store) }
        try expect(api.submitCalls.isEmpty && store.intents.count == 1, "lost prepare ACK triggered a submit or lost recovery")
        let original = store.intents[0]
        api.prepareHook = nil
        try await pending { try await send(store) }
        try expect(api.submitCalls.isEmpty && api.prepareCalls.count == 1, "ordinary send resumed a recovered prepared intent")
        _ = try await store.resume(original)
        try expect(api.submitCalls.map(\.clientRequestID) == [original.id], "explicit resume did not reuse original request")
    }

    static func testSubmitLostACKQueriesAccepted() async throws {
        let api = FixtureAPI()
        api.submitHook = { request in
            api.rows[request.clientRequestID] = record(request, state: .accepted, canSubmit: false, archived: true)
            throw URLError(.networkConnectionLost)
        }
        let store = makeStore(api)
        let result = try await send(store)
        try expect(result.state == .accepted && result.archiveConfirmed, "lost ACK did not recover accepted outcome")
        try expect(api.prepareCalls.count == 1 && api.submitCalls.count == 1 && api.getCalls.count == 1, "lost ACK retried a mutation")
        try expect(store.intents.isEmpty && !store.sending, "terminal result left stale recovery UI")
    }

    static func testUnknownRestartOnlyQueriesAndExplicitNewID() async throws {
        let api = FixtureAPI()
        api.submitHook = { request in
            api.rows[request.clientRequestID] = record(request, state: .unknown, canSubmit: false)
            throw URLError(.timedOut)
        }
        let first = makeStore(api)
        try await pending { try await send(first) }
        let oldID = api.submitCalls[0].clientRequestID
        let restarted = makeStore(api)
        try await pending { try await send(restarted) }
        try expect(api.prepareCalls.count == 1 && api.submitCalls.count == 1, "restart replayed an unknown outcome")
        try expect(restarted.intents.first?.id == oldID, "restart lost original pending ID")
        api.submitHook = nil
        let result = try await send(restarted, newRequest: true)
        try expect(result.clientRequestID != oldID && UUID(uuidString: result.clientRequestID) != nil, "explicit new send reused old request ID")
        try expect(api.submitCalls.count == 2, "explicit new send did not submit exactly once")
    }

    static func testArchiveUnknownAndExpiredBindingNeverResubmit() async throws {
        for state: DeviceSMSSendState in [.accepted, .prepared] {
            let api = FixtureAPI()
            let row = record(request("existing-\(state.rawValue)"), state: state, canSubmit: false, archived: false)
            api.rows[row.id] = row
            let store = makeStore(api)
            try await pending { try await send(store) }
            try await pending { try await store.resume(row) }
            try expect(api.prepareCalls.isEmpty && api.submitCalls.isEmpty && api.getCalls.count == 2, "archive-unknown or expired binding was resubmitted")
        }
    }

    static func testRestartWithServerBoundSubscriptionDoesNotReplay() async throws {
        let api = FixtureAPI()
        api.prepareHook = { request in
            let bound = DeviceSMSIntentRequest(clientRequestID: request.clientRequestID,
                                               phone: request.phone, message: request.message,
                                               device: request.device, expectedSubscriptionID: subscription)
            let row = record(bound)
            api.rows[row.id] = row
            return row
        }
        api.submitHook = { request in
            api.rows[request.clientRequestID] = record(request, state: .unknown, canSubmit: false)
            throw URLError(.networkConnectionLost)
        }
        let first = makeStore(api)
        try await pending {
            try await first.send(phone: phone, message: message, device: "reader", subscriptionID: nil)
        }
        let original = api.submitCalls[0].clientRequestID
        let restarted = makeStore(api)
        try await pending {
            try await restarted.send(phone: phone, message: message, device: "reader", subscriptionID: nil)
        }
        try expect(api.prepareCalls.count == 1 && api.submitCalls.count == 1,
                   "unknown subscription at UI startup bypassed server-bound pending request")
        try expect(restarted.intents.first?.id == original && api.getCalls.last == original,
                   "restart failed to query the original server-bound identity")
    }

    static func testBackendStopDuringPrepare() async throws {
        let api = FixtureAPI()
        let gate = FixtureGate<DeviceSMSIntent>()
        api.prepareHook = { _ in await gate.wait() }
        let store = makeStore(api)
        let task = Task { try await send(store) }
        try await waitUntil { gate.entered }
        store.setAvailable(false)
        gate.release(record(api.prepareCalls[0]))
        do { _ = try await task.value; throw FixtureFailure(description: "stopped prepare succeeded") }
        catch is CancellationError {}
        try expect(api.submitCalls.isEmpty && api.getCalls.isEmpty, "stale prepared ACK triggered another request")
        try expect(!store.sending && store.error == nil && store.intents.isEmpty, "stale prepared ACK mutated stopped store")
    }

    static func testBackendRestartDuringRecoveryQuery() async throws {
        let api = FixtureAPI()
        let gate = FixtureGate<DeviceSMSIntent>()
        api.prepareHook = { _ in throw URLError(.networkConnectionLost) }
        api.getHook = { _ in await gate.wait() }
        let store = makeStore(api)
        let task = Task { try await send(store) }
        try await waitUntil { gate.entered }
        store.setAvailable(false)
        store.setAvailable(true)
        gate.release(record(api.prepareCalls[0], state: .unknown, canSubmit: false))
        do { _ = try await task.value; throw FixtureFailure(description: "stale recovery query succeeded") }
        catch is CancellationError {}
        try expect(store.error == nil && store.intents.isEmpty && !store.sending, "old recovery query polluted new backend lifecycle")
        try expect(api.submitCalls.isEmpty, "recovery query submitted SMS")
    }

    static func testConcurrentSendHasOneMutation() async throws {
        let api = FixtureAPI()
        let gate = FixtureGate<DeviceSMSIntent>()
        api.prepareHook = { request in
            api.rows[request.clientRequestID] = record(request)
            return await gate.wait()
        }
        let store = makeStore(api)
        await store.refresh()
        let task = Task { try await send(store) }
        try await waitUntil { gate.entered }
        try await pending { try await send(store) }
        try expect(api.prepareCalls.count == 1, "concurrent send prepared two operations")
        gate.release(record(api.prepareCalls[0]))
        _ = try await task.value
        try expect(api.submitCalls.count == 1 && !store.sending, "concurrent send duplicated submission")
    }

    static func testPagingLoadsAllAndRejectsInvalidCursors() async throws {
        let api = FixtureAPI()
        for i in 0..<201 {
            let row = record(request("page-\(String(format: "%04d", i))"), state: .unknown, canSubmit: false)
            api.rows[row.id] = row
        }
        let store = makeStore(api)
        await store.refresh()
        try expect(store.intents.count == 201 && api.listCalls.count == 3 && store.error == nil, "only first page of recovery history loaded")
        api.listHook = { _ in DeviceSMSIntentPage(intents: [], nextCursor: "repeat") }
        await store.refresh()
        try expect(store.error != nil && store.intents.count == 201 && !store.loading, "cyclic cursor erased valid state or looped")
        let duplicate = record(request("duplicate"), state: .unknown, canSubmit: false)
        api.listHook = { _ in DeviceSMSIntentPage(intents: [duplicate, duplicate], nextCursor: nil) }
        await store.refresh()
        try expect(store.error != nil && store.intents.count == 201, "duplicate IDs accepted as valid recovery snapshot")
    }

    static func testStaleAndMismatchedQueryCannotOverwriteRefresh() async throws {
        let api = FixtureAPI()
        let original = record(request("query-id"), state: .unknown, canSubmit: false)
        api.rows[original.id] = original
        let store = makeStore(api)
        await store.refresh()
        let gate = FixtureGate<DeviceSMSIntent>()
        api.getHook = { _ in await gate.wait() }
        let task = Task { await store.query(original) }
        try await waitUntil { gate.entered }
        await store.refresh()
        gate.release(record(original.request, state: .accepted, canSubmit: false, archived: true))
        let result = await task.value
        try expect(result == nil && store.intents.first?.state == .unknown, "superseded query removed newer recovery result")
        api.getHook = { _ in
            record(DeviceSMSIntentRequest(clientRequestID: original.id, phone: "+19999999999", message: message,
                                          device: "reader", expectedSubscriptionID: subscription), state: .accepted,
                   canSubmit: false, archived: true)
        }
        let mismatch = await store.query(original)
        try expect(mismatch == nil && store.intents.first?.state == .unknown, "mismatched payload/target changed recovery state")
        try expect(api.prepareCalls.isEmpty && api.submitCalls.isEmpty, "query path performed mutation")
    }

    static func testClearPersistsWithoutGrantingResend() async throws {
        let suite = "SwitchSMSRecoveryTests-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let api = FixtureAPI()
        let row = record(request("clear-test"), state: .unknown, canSubmit: false)
        api.rows[row.id] = row
        let store = DeviceSMSSendStore(api: api, preferences: defaults)
        store.setAvailable(true)
        await store.refresh()
        store.dismiss(row)
        try expect(store.intents.isEmpty, "single remove did not hide record")
        let restarted = DeviceSMSSendStore(api: api, preferences: defaults)
        restarted.setAvailable(true)
        await restarted.refresh()
        try expect(restarted.intents.isEmpty, "dismissed record returned after restart")
        try await pending { try await send(restarted) }
        try expect(api.submitCalls.isEmpty && api.prepareCalls.isEmpty, "clear authorized duplicate SMS")
        let another = record(request("another"), state: .unknown, canSubmit: false)
        api.rows[another.id] = another
        await restarted.refresh()
        try expect(restarted.intents.count == 1, "clear hid a new request")
        restarted.clearRecoveryRecords()
        await restarted.refresh()
        try expect(restarted.intents.isEmpty, "clear-all did not persist")
    }

    static func main() async throws {
        let tests: [(String, () async throws -> Void)] = [
            ("clear preserves durable dispatch safety", testClearPersistsWithoutGrantingResend),
            ("prepare failure keeps ID", testPrepareFailureRetainsRequestID),
            ("prepare lost ACK needs explicit resume", testPrepareLostACKNeedsExplicitResume),
            ("submit lost ACK queries accepted", testSubmitLostACKQueriesAccepted),
            ("unknown restart and explicit new UUID", testUnknownRestartOnlyQueriesAndExplicitNewID),
            ("archive unknown and expired binding", testArchiveUnknownAndExpiredBindingNeverResubmit),
            ("server-bound subscription after restart", testRestartWithServerBoundSubscriptionDoesNotReplay),
            ("backend stop during prepare", testBackendStopDuringPrepare),
            ("backend restart during recovery query", testBackendRestartDuringRecoveryQuery),
            ("concurrent send", testConcurrentSendHasOneMutation),
            ("pagination and invalid cursor", testPagingLoadsAllAndRejectsInvalidCursors),
            ("stale and mismatched queries", testStaleAndMismatchedQueryCannotOverwriteRefresh)
        ]
        var failed: [String] = []
        for (name, test) in tests {
            do { try await test(); print("PASS \(name)") }
            catch { failed.append(name); print("FAIL \(name): \(error)") }
        }
        guard failed.isEmpty else { throw FixtureFailure(description: "Failed device send fixtures: \(failed.joined(separator: ", "))") }
        print("Device SMS send: \(tests.count) isolated recovery groups passed; no live API or hardware")
    }
}
