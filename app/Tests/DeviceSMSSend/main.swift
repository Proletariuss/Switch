import Foundation
import Darwin

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
                    canSubmit: canSubmit, createdAtMS: 100, updatedAtMS: 101,
                    identityVersion: request.identityVersion, intentNonce: request.intentNonce,
                    expectedSubscriptionID: request.identityVersion == 2 ? request.expectedSubscriptionID : nil)
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
    let journalDirectory = FileManager.default.temporaryDirectory.appendingPathComponent("device-sms-fixture-" + UUID().uuidString)
    var authorityCalls = 0
    var highWater: UInt64 = 0
    var reserveBytes: [Data] = []
    var prepareBytes: [Data] = []
    var submitBytes: [Data] = []
    var reserveHook: ((DeviceSMSReserveRequest) async throws -> DeviceSMSIntent)?
    var loseReserveACK = false
    var reservations: [String: DeviceSMSReserveRequest] = [:]
    var rows: [String: DeviceSMSIntent] = [:]
    var prepareCalls: [DeviceSMSIntentRequest] = []
    var submitCalls: [DeviceSMSIntentRequest] = []
    var reconcileCalls: [DeviceSMSIntentRequest] = []
    var reconcileHook: ((DeviceSMSIntentRequest) async throws -> Void)?
    var getCalls: [String] = []
    var listCalls: [String?] = []
    var prepareHook: ((DeviceSMSIntentRequest) async throws -> DeviceSMSIntent)?
    var submitHook: ((DeviceSMSIntentRequest) async throws -> Void)?
    var getHook: ((String) async throws -> DeviceSMSIntent)?
    var listHook: ((String?) async throws -> DeviceSMSIntentPage)?

    func authority() async throws -> DeviceSMSAuthorityStatus {
        authorityCalls += 1
        return DeviceSMSSendTests.status(high: highWater)
    }
    func reserve(bytes: Data) async throws -> DeviceSMSIntent {
        reserveBytes.append(bytes)
        let reservation = try JSONDecoder().decode(DeviceSMSReserveRequest.self, from: bytes)
        if let reserveHook { return try await reserveHook(reservation) }
        if let existing = reservations[reservation.clientRequestID] {
            guard existing == reservation else { throw DeviceSMSIdentityRefusal() }
            return rows[reservation.clientRequestID]!
        }
        guard UInt64(reservation.sequence) == highWater + 1 else { throw DeviceSMSIdentityRefusal() }
        highWater += 1
        reservations[reservation.clientRequestID] = reservation
        let value = record(reservation.request, state: .reserved, canSubmit: false)
        rows[value.id] = value
        if loseReserveACK { throw URLError(.networkConnectionLost) }
        return value
    }
    func prepareV2(bytes: Data) async throws -> DeviceSMSIntent {
        prepareBytes.append(bytes)
        return try await prepare(JSONDecoder().decode(DeviceSMSIntentRequest.self, from: bytes))
    }
    func submitV2(bytes: Data) async throws {
        submitBytes.append(bytes)
        try await submit(JSONDecoder().decode(DeviceSMSIntentRequest.self, from: bytes))
    }
    func reconcileArchiveV2(bytes: Data) async throws {
        try await reconcileArchive(JSONDecoder().decode(DeviceSMSIntentRequest.self, from: bytes))
    }
    func cancelReservation(bytes: Data) async throws -> DeviceSMSIntent {
        let reservation = try JSONDecoder().decode(DeviceSMSReserveRequest.self, from: bytes)
        guard let previous = rows[reservation.clientRequestID], previous.state == .reserved,
              previous.intentNonce == reservation.intentNonce else { throw DeviceSMSIdentityRefusal() }
        let result = record(reservation.request, state: .cancelled, canSubmit: false)
        rows[result.id] = result
        return result
    }
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
        if let row = rows[request.clientRequestID], row.state != .reserved { return row }
        let row = record(request)
        rows[row.id] = row
        return row
    }
    func reconcileArchive(_ request: DeviceSMSIntentRequest) async throws {
        reconcileCalls.append(request)
        if let reconcileHook { return try await reconcileHook(request) }
        guard rows[request.clientRequestID]?.state == .accepted else { throw URLError(.badServerResponse) }
        rows[request.clientRequestID] = record(request, state: .accepted, canSubmit: false, archived: true)
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
        let store = DeviceSMSSendStore(api: api, journal: DeviceSMSIssuanceJournal(directory: api.journalDirectory))
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
        try expect(result.clientRequestID != oldID && result.clientRequestID.hasPrefix("m2:ds:"), "explicit new send reused old request ID")
        try expect(api.submitCalls.count == 2, "explicit new send did not submit exactly once")
    }

    static func testArchiveUnknownAndExpiredBindingNeverResubmit() async throws {
        for state: DeviceSMSSendState in [.accepted, .prepared] {
            let api = FixtureAPI()
            let row = record(request("existing-\(state.rawValue)"), state: state, canSubmit: false, archived: false)
            api.rows[row.id] = row
            let store = makeStore(api)
            try await pending { try await send(store) }
            if state == .accepted {
                let restored = try await store.resume(row)
                try expect(restored.archiveConfirmed && api.reconcileCalls == [row.request], "archive recovery changed identity")
            } else {
                try await pending { try await store.resume(row) }
                try expect(api.reconcileCalls.isEmpty, "expired prepared record entered archive recovery")
            }
            try expect(api.prepareCalls.isEmpty && api.submitCalls.isEmpty, "archive-unknown or expired binding was resubmitted")
        }
    }

    static func testRestartWithServerBoundSubscriptionDoesNotReplay() async throws {
        let api = FixtureAPI()
        api.prepareHook = { request in
            let bound = DeviceSMSIntentRequest(clientRequestID: request.clientRequestID,
                                               phone: request.phone, message: request.message,
                                               device: request.device, expectedSubscriptionID: subscription, identityVersion: request.identityVersion, intentNonce: request.intentNonce)
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

    static func testArchiveRecoveryLostACKAndRestartKeepOriginalIdentity() async throws {
        let api = FixtureAPI()
        let row = record(request("archive-recovery"), state: .accepted, canSubmit: false)
        api.rows[row.id] = row
        api.reconcileHook = { request in
            api.rows[request.clientRequestID] = record(request, state: .accepted, canSubmit: false, archived: true)
            throw URLError(.networkConnectionLost)
        }
        let store = makeStore(api)
        await store.refresh()
        let result = try await store.resume(row)
        try expect(result.archiveConfirmed && store.intents.isEmpty, "lost archive ACK did not query same receipt")
        try expect(api.reconcileCalls == [row.request] && api.submitCalls.isEmpty && api.prepareCalls.isEmpty,
                   "archive recovery acquired a physical send")
        let restarted = makeStore(api)
        await restarted.refresh()
        try expect(restarted.intents.isEmpty, "confirmed archive returned as unresolved after restart")
    }

    static func testArchiveRecoveryFailureAndMismatchNeverReissue() async throws {
        let api = FixtureAPI()
        let row = record(request("archive-failed"), state: .accepted, canSubmit: false)
        api.rows[row.id] = row
        api.reconcileHook = { _ in throw URLError(.timedOut) }
        let store = makeStore(api)
        await store.refresh()
        try await pending { try await store.resume(row) }
        try expect(store.intents.first?.id == row.id, "failed archive recovery lost original request")
        let changed = DeviceSMSIntentRequest(clientRequestID: row.id, phone: phone,
            message: "different", device: "reader", expectedSubscriptionID: subscription)
        api.rows[row.id] = record(changed, state: .accepted, canSubmit: false)
        try await pending { try await store.resume(row) }
        try expect(api.reconcileCalls.count == 1 && api.submitCalls.isEmpty && api.prepareCalls.isEmpty,
                   "mismatched archive observation granted a mutation")
    }

    static func status(high: UInt64 = 0) -> DeviceSMSAuthorityStatus {
        let authority = String(repeating: "a", count: 32)
        return DeviceSMSAuthorityStatus(identityVersion: 2, family: "device_sms", authorityID: authority,
            epoch: "1", issuedHighWater: String(high), nextSequence: high == UInt64.max ? nil : String(high + 1),
            nextClientRequestID: high == UInt64.max ? nil : DeviceSMSAuthorityStatus.identifier(authority: authority, epoch: 1, sequence: high + 1))
    }
    static func checkpoint(high: UInt64 = 0, body: String = message) throws -> DeviceSMSIssuanceCheckpoint {
        try .make(DeviceSMSReserveRequest.candidate(status: status(high: high), phone: phone,
            message: body, device: "reader", subscription: subscription))
    }
    static func testReserveACKLossReopensExactBytes() async throws {
        let api = FixtureAPI()
        defer { try? FileManager.default.removeItem(at: api.journalDirectory) }
        api.loseReserveACK = true
        try await pending { try await send(makeStore(api)) }
        try expect(api.prepareCalls.isEmpty && api.submitCalls.isEmpty && api.reserveBytes.count == 1,
                   "reserve ACK loss reached prepare/dispatch")
        let before = try DeviceSMSIssuanceJournal(directory: api.journalDirectory).entries()[0]
        api.loseReserveACK = false
        let result = try await send(makeStore(api))
        try expect(api.reserveBytes == [before.reserveBytes, before.reserveBytes], "restart did not replay exact reserve bytes")
        try expect(api.prepareBytes == [before.sendBytes] && api.submitBytes == [before.sendBytes], "prepare/claim changed saved business bytes")
        let remaining = try DeviceSMSIssuanceJournal(directory: api.journalDirectory).entries()
        try expect(result.archiveConfirmed && remaining.isEmpty && api.authorityCalls == 1, "terminal confirmation or original candidate recovery drifted")
    }
    static func testJournalDurabilityFailurePrecedesNetwork() async throws {
        for boundary in ["before-rename", "after-rename"] {
            let api = FixtureAPI()
            defer { try? FileManager.default.removeItem(at: api.journalDirectory) }
            let journal = DeviceSMSIssuanceJournal(directory: api.journalDirectory)
            _ = try journal.entries()
            journal.fault = { point in if point == boundary { throw DeviceSMSJournalError.unavailable } }
            let store = DeviceSMSSendStore(api: api, journal: journal)
            store.setAvailable(true)
            try await pending { try await send(store) }
            try expect(api.reserveBytes.isEmpty && api.prepareCalls.isEmpty && api.submitCalls.isEmpty,
                       "failed durable boundary allowed network mutation")
            journal.fault = nil
            try await pending { try await send(store) }
            try expect(api.reserveBytes.isEmpty, "failed lifetime recovered its own health")
            let reopened = DeviceSMSIssuanceJournal(directory: api.journalDirectory)
            let entries = try reopened.entries()
            try expect(entries.count == (boundary == "after-rename" ? 1 : 0), "interrupted replacement lost expected prior/next bytes")
            if let saved = entries.first {
                _ = try await send(makeStore(api))
                try expect(api.reserveBytes == [saved.reserveBytes], "reopen replaced interrupted exact candidate")
            }
        }
    }
    static func testCorruptionPermissionsAndMissingSnapshotFailClosed() async throws {
        for failure in ["corrupt", "missing", "symlink", "permissions"] {
            let api = FixtureAPI()
            defer { try? FileManager.default.removeItem(at: api.journalDirectory) }
            let journal = DeviceSMSIssuanceJournal(directory: api.journalDirectory)
            _ = try journal.insert(checkpoint(), explicitNew: false)
            let snapshot = api.journalDirectory.appendingPathComponent("issuance.json")
            let mode = try FileManager.default.attributesOfItem(atPath: snapshot.path)[.posixPermissions] as? NSNumber
            try expect(mode?.intValue == 0o600, "journal snapshot is not private")
            switch failure {
            case "corrupt": try Data("broken".utf8).write(to: snapshot)
            case "missing": try FileManager.default.removeItem(at: snapshot)
            case "permissions": try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: snapshot.path)
            default:
                try FileManager.default.removeItem(at: snapshot)
                try FileManager.default.createSymbolicLink(atPath: snapshot.path, withDestinationPath: "/dev/null")
            }
            try await pending { try await send(makeStore(api)) }
            try expect(api.authorityCalls == 0 && api.reserveBytes.isEmpty && api.submitCalls.isEmpty, "damaged journal created a new identity")
        }
    }
    static func testRetiredAndNonceConflictKeepOriginalEvidence() async throws {
        let api = FixtureAPI()
        defer { try? FileManager.default.removeItem(at: api.journalDirectory) }
        api.reserveHook = { _ in throw DeviceSMSIdentityRefusal() }
        let store = makeStore(api)
        try await pending { try await send(store) }
        let journal = DeviceSMSIssuanceJournal(directory: api.journalDirectory)
        let saved = try journal.entries()[0]
        try await pending { try await send(makeStore(api)) }
        try expect(api.reserveBytes.count == 1 && api.authorityCalls == 1, "retired identity was reissued or renumbered")
        var other = try saved.request
        other.intentNonce = String(repeating: "b", count: 32)
        api.rows[other.clientRequestID] = record(other, state: .accepted, canSubmit: false, archived: true)
        api.highWater = 1
        let restarted = makeStore(api)
        await restarted.refresh()
        let retained = try journal.entries()
        try expect(retained.count == 1 && retained[0].sameBytes(saved), "other nonce's terminal record cleared loser checkpoint")
        try await pending { try await send(restarted) }
        api.reserveHook = nil
        let newResult = try await send(restarted, newRequest: true)
        let after = try journal.entries()
        try expect(newResult.id != other.clientRequestID && after.count == 1 && after[0].sameBytes(saved), "explicit new message discarded or reused conflicting old evidence")
    }
    static func testLegacyPreparedOnlyQueries() async throws {
        let api = FixtureAPI()
        let legacy = record(request("legacy-pending"))
        api.rows[legacy.id] = legacy
        let store = makeStore(api)
        try await pending { try await send(store) }
        try await pending { try await store.resume(legacy) }
        try expect(api.authorityCalls == 0 && api.reserveBytes.isEmpty && api.prepareCalls.isEmpty && api.submitCalls.isEmpty,
                   "legacy pending upgraded or resubmitted")
    }
    static func testLostJournalCanOnlyCancelReservedNonce() async throws {
        let api = FixtureAPI()
        let saved = try checkpoint()
        let request = try saved.request
        api.rows[request.clientRequestID] = record(request, state: .reserved, canSubmit: false)
        let store = DeviceSMSSendStore(api: api)
        store.setAvailable(true)
        await store.refresh()
        try await pending { try await store.resume(api.rows[request.clientRequestID]!) }
        let result = try await store.cancelReservation(api.rows[request.clientRequestID]!)
        try expect(result.state == .cancelled && api.prepareCalls.isEmpty && api.submitCalls.isEmpty && api.reserveBytes.isEmpty,
                   "lost journal cancellation acquired issuance/dispatch permission")
        let unknown = record(request, state: .unknown, canSubmit: false)
        try await pending { try await store.cancelReservation(unknown) }
    }
    static func testJournalCapacityDoesNotEvictUnknown() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("sms-capacity-" + UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = DeviceSMSIssuanceJournal(directory: directory)
        for index in 0..<64 {
            let saved = try journal.insert(checkpoint(high: UInt64(index), body: "message-\(index)"), explicitNew: true)
            _ = try journal.advance(saved, to: .unresolved)
        }
        do { _ = try journal.insert(checkpoint(high: 64), explicitNew: true); throw FixtureFailure(description: "journal exceeded bound") }
        catch DeviceSMSJournalError.capacity {}
        let rows = try DeviceSMSIssuanceJournal(directory: directory).entries()
        try expect(rows.count == 64 && rows.allSatisfy { $0.phase == .unresolved }, "capacity evicted uncertain evidence")
    }
    static func testTwoProcessesClaimOneLocalCheckpoint() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("sms-flock-" + UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = DeviceSMSIssuanceJournal(directory: directory)
        _ = try journal.insert(checkpoint(), explicitNew: false)
        let executable = URL(fileURLWithPath: CommandLine.arguments[0])
        let children = (0..<2).map { _ -> Process in
            let child = Process()
            child.executableURL = executable
            child.arguments = ["--journal-claim", directory.path]
            return child
        }
        for child in children { try child.run() }
        for child in children { child.waitUntilExit() }
        try expect(children.filter { $0.terminationStatus == 0 }.count == 1,
                   "two processes acquired the same checkpoint dispatch permission")
        let rows = try journal.entries()
        try expect(rows.count == 1 && rows[0].phase == .dispatched, "cross-process claim not durable")
    }
    static func journalWorker() throws {
        let journal = DeviceSMSIssuanceJournal(directory: URL(fileURLWithPath: CommandLine.arguments[2]))
        do {
            let original = try journal.entries()[0]
            let prepared = try journal.advance(original, to: .prepared)
            _ = try journal.advance(prepared, to: .dispatched)
        } catch DeviceSMSJournalError.conflict { exit(10) }
    }
    static func main() async throws {
        if CommandLine.arguments.count == 3 && CommandLine.arguments[1] == "--journal-claim" {
            try journalWorker()
            return
        }
        let tests: [(String, () async throws -> Void)] = [
            ("reserve ACK loss reopens exact bytes", testReserveACKLossReopensExactBytes),
            ("journal durability faults precede network", testJournalDurabilityFailurePrecedesNetwork),
            ("journal corruption/missing/permissions", testCorruptionPermissionsAndMissingSnapshotFailClosed),
            ("retired and nonce conflicts retain evidence", testRetiredAndNonceConflictKeepOriginalEvidence),
            ("legacy prepared remains read-only", testLegacyPreparedOnlyQueries),
            ("lost journal reserved cancellation", testLostJournalCanOnlyCancelReservedNonce),
            ("journal bounds retain unknown", testJournalCapacityDoesNotEvictUnknown),
            ("two-process journal claim", testTwoProcessesClaimOneLocalCheckpoint),
            ("archive recovery lost ACK and restart", testArchiveRecoveryLostACKAndRestartKeepOriginalIdentity),
            ("archive recovery failure and mismatch", testArchiveRecoveryFailureAndMismatchNeverReissue),
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
