import Foundation

struct APIClient {
    init(timeoutInterval: TimeInterval) {}
    func get<T: Decodable>(_ path: String) async throws -> T { throw Failure.unexpectedNetwork }
}
enum Failure: Error { case failed(String), unexpectedNetwork }
func expect(_ value: @autoclosure () -> Bool, _ message: String) throws {
    if !value() { throw Failure.failed(message) }
}
let degradedJSON = """
{"ok":true,"private_path":"private-source-must-not-export","sms_store_health":{
"schema_version":1,"degraded":true,
"device_send":{"state":"unavailable","reason":"store_corrupt","restart_required":true},
"nwait":{"state":"ready","reason":null,"restart_required":false}}}
"""
func decode(_ value: String) throws -> SMSRuntimeHealthResponse {
    try JSONDecoder().decode(SMSRuntimeHealthResponse.self, from: Data(value.utf8))
}
actor ControlledAPI: SMSStoreHealthAPI {
    private var pending: [CheckedContinuation<SMSRuntimeHealthResponse, Error>] = []
    func read() async throws -> SMSRuntimeHealthResponse {
        try await withCheckedThrowingContinuation { pending.append($0) }
    }
    func count() -> Int { pending.count }
    func complete(_ index: Int, _ response: SMSRuntimeHealthResponse) { pending[index].resume(returning: response) }
    func fail(_ index: Int) { pending[index].resume(throwing: Failure.failed("sensitive-path-or-message")) }
}
func wait(_ api: ControlledAPI, _ count: Int) async throws {
    for _ in 0..<10000 {
        if await api.count() >= count { return }
        await Task.yield()
    }
    throw Failure.failed("fixture did not reach API")
}
@main enum Tests {
    @MainActor static func main() async throws {
        let degraded = try decode(degradedJSON)
        try expect(degraded.smsStoreHealth?.deviceSend.state == .unavailable, "unavailable store looked healthy")
        try expect(degraded.smsStoreHealth?.nwait.state == .ready, "healthy store was incorrectly disabled")
        let exported = String(decoding: try JSONEncoder().encode(degraded.smsStoreHealth), as: UTF8.self)
        try expect(!exported.contains("private-source"), "unknown fields leaked")
        let old = try decode("{\"ok\":true,\"backend\":\"rust\"}")
        try expect(old.smsStoreHealth == nil, "old backend invented healthy metadata")
        for invalid in [
            degradedJSON.replacingOccurrences(of: "\"degraded\":true", with: "\"degraded\":false"),
            degradedJSON.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":2"),
            degradedJSON.replacingOccurrences(of: "store_corrupt", with: "sensitive-path-or-message"),
            degradedJSON.replacingOccurrences(of: "\"restart_required\":true", with: "\"restart_required\":false"),
            degradedJSON.replacingOccurrences(of: "\"reason\":null", with: "\"reason\":\"store_io_failed\"")
        ] {
            do { _ = try decode(invalid); throw Failure.failed("inconsistent or unsafe diagnostic accepted") }
            catch is DecodingError {}
        }
        print("PASS: independent store health, legacy metadata, closed diagnostic codes and consistency checks")

        let api = ControlledAPI()
        let store = SMSStoreHealthStore(api: api)
        let previous = Task { await store.refresh() }
        try await wait(api, 1)
        let latest = Task { await store.refresh() }
        try await wait(api, 2)
        await api.complete(1, degraded); await latest.value
        await api.complete(0, old); await previous.value
        try expect(store.snapshot?.degraded == true && !store.loading, "late response overwrote current degraded state")
        let cancelled = Task { await store.refresh() }
        try await wait(api, 3)
        cancelled.cancel(); store.invalidate()
        await api.complete(2, degraded); await cancelled.value
        try expect(store.snapshot == nil && !store.loading, "stopped backend kept stale health")
        let failed = Task { await store.refresh() }
        try await wait(api, 4)
        await api.fail(3); await failed.value
        try expect(store.snapshot == nil && store.detail == "短信存储状态读取未完成，请重新查询。", "failure leaked details or retained healthy state")
        let legacy = Task { await store.refresh() }
        try await wait(api, 5)
        await api.complete(4, old); await legacy.value
        try expect(store.detail == "当前后端未提供短信存储健康信息。", "missing metadata reported as healthy")
        print("PASS: response generation, cancellation, backend restart and failed/legacy queries never publish stale health")
    }
}
