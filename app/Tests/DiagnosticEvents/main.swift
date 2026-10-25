import Foundation

struct APIClient {
    init(timeoutInterval: TimeInterval) {}
    func get<T: Decodable>(_ path: String) async throws -> T { throw TestFailure.failed("live API invoked") }
}
enum TestFailure: Error { case failed(String) }
func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw TestFailure.failed(message) }
}
func fixture(_ sequence: UInt64 = 1) throws -> DiagnosticEventSnapshot {
    try JSONDecoder().decode(DiagnosticEventSnapshot.self, from: Data("""
    {"schema_version":1,"capacity":256,"latest_sequence":\(sequence),"private_raw":"synthetic-token-998877",
    "events":[{"sequence":\(sequence),"time_ms":1791000000000,"operation":"nwait_session",
    "correlation_id":"0123456789abcdef0123456789abcdef","generation":3,"stage":"ready",
    "code":"ready","duration_ms":125,"result":"succeeded","sms_body":"synthetic-otp-123456"}]}
    """.utf8))
}
actor ControlledAPI: DiagnosticEventAPI {
    private var waiting: [CheckedContinuation<DiagnosticEventSnapshot, Error>] = []
    func snapshot() async throws -> DiagnosticEventSnapshot {
        try await withCheckedThrowingContinuation { waiting.append($0) }
    }
    func count() -> Int { waiting.count }
    func complete(_ index: Int, _ value: DiagnosticEventSnapshot) { waiting[index].resume(returning: value) }
    func fail(_ index: Int) { waiting[index].resume(throwing: TestFailure.failed("raw-secret-phone-447700900123")) }
}
func wait(_ api: ControlledAPI, _ count: Int) async throws {
    for _ in 0..<10000 {
        if await api.count() >= count { return }
        await Task.yield()
    }
    throw TestFailure.failed("fixture did not reach API")
}
@main enum DiagnosticEventsTests {
    @MainActor static func main() async throws {
        let snapshot = try fixture()
        let encoded = String(decoding: try snapshot.exportData(), as: UTF8.self)
        try expect(!encoded.contains("synthetic") && !encoded.contains("private_raw") && !encoded.contains("sms_body"), "unknown response extras escaped export")
        let base = String(decoding: try snapshot.exportData(), as: UTF8.self)
        for mutated in [base.replacingOccurrences(of: "nwait_session", with: "phone-447700900123"),
                        base.replacingOccurrences(of: "0123456789abcdef0123456789abcdef", with: "iccid-123"),
                        base.replacingOccurrences(of: "\"capacity\" : 256", with: "\"capacity\" : 513"),
                        base.replacingOccurrences(of: "\"latest_sequence\" : 1", with: "\"latest_sequence\" : 7")] {
            do {
                _ = try JSONDecoder().decode(DiagnosticEventSnapshot.self, from: Data(mutated.utf8))
                throw TestFailure.failed("invalid diagnostic metadata accepted")
            } catch is DecodingError {}
        }
        print("PASS: closed DTO rejects arbitrary values, invalid ring metadata and sensitive correlation; export drops extras")

        let api = ControlledAPI()
        let store = DiagnosticEventStore(api: api)
        let old = Task { await store.refresh() }
        try await wait(api, 1)
        let latest = Task { await store.refresh() }
        try await wait(api, 2)
        await api.complete(1, try fixture(2)); await latest.value
        await api.complete(0, snapshot); await old.value
        try expect(store.snapshot?.latestSequence == 2 && !store.loading, "stale response overwrote newest snapshot")

        let cancelled = Task { await store.refresh() }
        try await wait(api, 3)
        cancelled.cancel(); store.invalidate()
        await api.complete(2, try fixture(3)); await cancelled.value
        try expect(store.snapshot?.latestSequence == 2 && !store.loading, "cancelled request published or remained busy")

        let failed = Task { await store.refresh() }
        try await wait(api, 4)
        await api.fail(3); await failed.value
        try expect(store.error == "安全事件读取未完成，请稍后重试。" && store.snapshot?.latestSequence == 2 && !store.loading, "raw error leaked or last snapshot lost")
        print("PASS: newest request wins; cancellation suppresses late publish; fixed failure text retains last snapshot")
    }
}
