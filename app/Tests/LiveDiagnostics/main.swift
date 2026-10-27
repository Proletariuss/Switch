import AppKit
import Foundation
import SwiftUI

struct APIClient {
    init(timeoutInterval: TimeInterval) {}
    func getData(_ path: String) async throws -> Data { throw Failure.failed("live API invoked") }
}
enum Failure: Error { case failed(String) }
func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw Failure.failed(message) }
}
let firstRuntime = String(repeating: "a", count: 32)
let secondRuntime = String(repeating: "b", count: 32)
func record(_ sequence: UInt64, runtime: String = firstRuntime) -> [String: Any] {
    ["schema_version": 1, "runtime_id": runtime, "sequence": sequence, "time_ms": sequence,
     "event": ["kind": "api", "route": "/api/status", "method": "GET", "status": 200,
               "code": "synthetic-secret-447700900123", "duration_ms": 1,
               "correlation_id": String(repeating: "c", count: 32), "raw": "private"]]
}
func fixture(_ sequences: [UInt64] = [1], runtime: String = firstRuntime,
             mutate: (inout [String: Any]) -> Void = { _ in }) throws -> Data {
    var value: [String: Any] = [
        "schema_version": 1, "view": "live", "generated_at_ms": 1, "runtime_id": runtime,
        "version": "3.56", "build": "356", "source_digest": String(repeating: "a", count: 64),
        "limits": ["queue_capacity":256, "max_record_bytes":4096, "segment_bytes":2097152,
                   "segments":4, "export_bytes":2097152, "max_events":256,
                   "memory_bytes":262144, "response_bytes":294912],
        "counters": ["accepted":256, "written":0, "dropped":0, "write_failures":0,
                     "invalid_records":0, "read_failures":0, "live_evicted":0,
                     "live_missed":0, "retained_bytes":100],
        "events": sequences.map { record($0, runtime: runtime) },
        "warnings": ["live_current_runtime_only", "best_effort_not_lossless", "synthetic-secret"],
        "private": "secret",
    ]
    mutate(&value)
    return try JSONSerialization.data(withJSONObject: value)
}
actor ControlledAPI: LiveDiagnosticAPI {
    private var waiting: [CheckedContinuation<Data, Error>] = []
    func snapshot() async throws -> Data {
        try await withCheckedThrowingContinuation { waiting.append($0) }
    }
    func count() -> Int { waiting.count }
    func complete(_ index: Int, _ data: Data) { waiting[index].resume(returning: data) }
    func fail(_ index: Int) { waiting[index].resume(throwing: Failure.failed("raw-secret-phone")) }
}
@MainActor func waitUntil(_ predicate: @MainActor () async -> Bool) async throws {
    for _ in 0..<2000 {
        if await predicate() { return }
        try await Task.sleep(nanoseconds: 1_000_000)
    }
    throw Failure.failed("condition did not become true")
}

@main enum LiveDiagnosticsTests {
    @MainActor static func main() async throws {
        let client = ClientDiagnostics()
        try expect(client.liveSnapshot().records.isEmpty, "new GUI session inherited historical records")
        for _ in 0..<300 { client.lifecycle(component: "application", stage: "started") }
        client.lifecycle(component: "synthetic-secret", stage: "started")
        let local = client.liveSnapshot()
        try expect(local.records.count == 256 && local.evicted == 44
            && local.records.first?.sequence == 45 && local.records.last?.sequence == 300,
            "current-session in-memory ring is unbounded or retained invalid fields")
        // No writer/start/root was installed: this path cannot read or create a file.
        let lifecycleRows = LiveDiagnosticSnapshot.clientRows(local)
        try expect(lifecycleRows.allSatisfy { !$0.detail.contains("HTTP") && !$0.warning
            && $0.title.contains("已启动") }, "normal lifecycle was mislabeled as an incomplete HTTP request")
        let clean = try LiveDiagnosticSnapshot.decode(fixture([3, 1, 2, 2]))
        try expect(clean.rows.count == 3, "repeated backend sequence was not deduplicated")
        let text = clean.rows.map { $0.title + $0.detail }.joined() + clean.notices.joined()
        try expect(!text.contains("synthetic") && !text.contains("private"), "untrusted fields reached live UI")
        for input in [
            try fixture { $0["view"] = "export" },
            try fixture { $0["events"] = [1] },
            try fixture { $0["events"] = [NSNull()] },
            try fixture { $0["events"] = ["string"] },
            try fixture { $0["events"] = [[]] },
            try fixture(Array(1...257).map(UInt64.init)),
            try fixture { $0["events"] = [record(1, runtime: secondRuntime)] },
            try fixture { value in
                var counters = value["counters"] as! [String: Int]
                counters["retained_bytes"] = 262145
                value["counters"] = counters
            },
            Data(repeating: 0x20, count: 294913)
        ] {
            do {
                _ = try LiveDiagnosticSnapshot.decode(input)
                throw Failure.failed("invalid live boundary accepted")
            } catch LiveDiagnosticSnapshot.Failure.invalid {}
            catch is DecodingError {}
            catch let error as Failure { throw error }
            catch {}
        }
        let recoveryOnly = try LiveDiagnosticSnapshot.decode(fixture([]) { value in
            value["module_recovery"] = [
                "schema_version": 1, "observation": "last_observed",
                "state": "legacy_recovery_required", "reason_codes": ["legacy_call_mode_lock"],
                "historical_recovery_confirmed": false,
                "startup": ["metadata":"failed", "reconciliation":"not_observed",
                            "native_inspection":"not_observed", "cleanup":"unconfirmed"],
                "private": "synthetic-secret"
            ]
        })
        try expect(recoveryOnly.rows.isEmpty
            && recoveryOnly.recovery?.contains("legacy_call_mode_lock") == true
            && recoveryOnly.recovery?.contains("元数据 失败") == true
            && recoveryOnly.recovery?.contains("synthetic") == false,
            "recovery observation disappeared without events or exposed unknown fields")
        let notices = try LiveDiagnosticSnapshot.decode(fixture { value in
            value["warnings"] = ["live_window_truncated", "live_records_missed", "synthetic-secret"]
        }).notices.joined()
        try expect(notices.contains("移出") && notices.contains("未能入队") && !notices.contains("synthetic"),
                   "live retention/missed notices were lost or copied unknown warning")
        print("PASS: memory-only client window, closed live DTO, byte/record/runtime bounds and dedup")

        let api = ControlledAPI()
        let store = LiveDiagnosticStore(api: api, client: { local }, interval: 10_000_000)
        store.setLifecycle(visible: true, running: true, instance: 1)
        try await waitUntil { await api.count() == 1 }
        // PID changes while state still reads running; do not overlap old/new requests.
        store.setLifecycle(visible: true, running: true, instance: 2)
        try expect(store.runtimeID == nil && store.updatedAt == nil, "old backend observation survived instance change")
        await api.complete(0, try fixture([100]))
        try await waitUntil { await api.count() == 2 }
        try expect(store.rows.allSatisfy { $0.source.hasPrefix("客户端") }, "old response published into replacement instance")
        await api.complete(1, try fixture(Array(1...256).map(UInt64.init), runtime: secondRuntime))
        try await waitUntil { store.runtimeID == secondRuntime && store.rows.count == 500 }
        try expect(Set(store.rows.map(\.id)).count == store.rows.count, "merged list has duplicate IDs")
        try expect(zip(store.rows, store.rows.dropFirst()).allSatisfy { pair in pair.0.timeMS <= pair.1.timeMS }, "unordered backend response was not sorted")
        try await waitUntil { await api.count() == 3 }
        await api.fail(2)
        try await waitUntil { store.updatedAt == nil && store.status.contains("读取失败") }
        try expect(!store.status.contains("raw-secret") && store.rows.allSatisfy { $0.source == "客户端" },
                   "read failure leaked raw error or retained backend data as current")
        try await waitUntil { await api.count() == 4 }
        store.setLifecycle(visible: false, running: true, instance: 2)
        await api.complete(3, try fixture([999], runtime: secondRuntime))
        try await Task.sleep(nanoseconds: 30_000_000)
        let count = await api.count()
        try expect(count == 4 && store.updatedAt == nil, "hidden view polled or accepted a late response")
        store.setLifecycle(visible: true, running: false, instance: 0)
        try await Task.sleep(nanoseconds: 30_000_000)
        let stoppedCount = await api.count()
        try expect(stoppedCount == 4, "stopped backend received a poll")
        store.setLifecycle(visible: false, running: false)
        print("PASS: serial PID/revision ownership, late cancellation, failure unknown state and 500-row bound")

        let releaseAPI = ControlledAPI()
        var transient: LiveDiagnosticStore? = LiveDiagnosticStore(api: releaseAPI, client: { local })
        weak var releasedStore = transient
        transient?.setLifecycle(visible: true, running: true, instance: 1)
        try await waitUntil { await releaseAPI.count() == 1 }
        transient = nil
        try expect(releasedStore != nil, "finite in-flight poll lost its owner before retiring")
        await releaseAPI.complete(0, try fixture())
        try await waitUntil { releasedStore == nil }
        let releasedCalls = await releaseAPI.count()
        try expect(releasedCalls == 1, "dropped store started another poll")
        print("PASS: dropping the store releases it after the finite poll without onDisappear")

        let uiAPI = ControlledAPI()
        let uiStore = LiveDiagnosticStore(api: uiAPI, client: { local }, interval: 10_000_000)
        let backend = BackendProcess()
        _ = NSApplication.shared
        let host = NSHostingView(rootView: SupportDiagnosticsView(store: uiStore).environmentObject(backend))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 650),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        window.orderFront(nil)
        host.layoutSubtreeIfNeeded()
        try await waitUntil { await uiAPI.count() == 1 }
        await uiAPI.complete(0, try fixture())
        try await waitUntil { uiStore.updatedAt != nil }
        backend.state = .stopped
        try await waitUntil { uiStore.updatedAt == nil && uiStore.status.contains("后端未运行") }
        // If a poll raced the state change, retire its actual owner before closing.
        let outstanding = await uiAPI.count()
        if outstanding > 1 { await uiAPI.complete(1, try fixture([2])) }
        window.contentView = nil
        window.orderOut(nil)
        try await waitUntil { uiStore.status == "实时读取已暂停。" }
        print("PASS: actual SupportDiagnosticsView mounts, observes backend exit, and cancels when hidden")
    }
}
