import AppKit
import Foundation
import SwiftUI
import Vision

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

@MainActor func accessibilityText(in element: Any, depth: Int = 0) -> String {
    guard depth < 32, let node = element as? any NSAccessibilityProtocol else { return "" }
    let text = [node.accessibilityLabel() ?? "", node.accessibilityValue() as? String ?? ""]
    return (text + (node.accessibilityChildren() ?? []).map {
        accessibilityText(in: $0, depth: depth + 1)
    }).joined(separator: "\n")
}

@discardableResult
@MainActor func saveScreenshot(_ host: NSView, named name: String) throws -> Data {
    guard let output = ProcessInfo.processInfo.environment["LIVE_DIAGNOSTIC_SCREENSHOT_DIR"],
          let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) else {
        throw Failure.failed("synthetic screenshot destination or bitmap unavailable")
    }
    host.cacheDisplay(in: host.bounds, to: bitmap)
    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        throw Failure.failed("synthetic hosted screenshot could not be encoded")
    }
    let directory = URL(fileURLWithPath: output, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    try png.write(to: directory.appendingPathComponent(name), options: [.atomic])
    return png
}

@MainActor func scrollViews(in view: NSView) -> [NSScrollView] {
    let current = (view as? NSScrollView).map { [$0] } ?? []
    return current + view.subviews.flatMap { scrollViews(in: $0) }
}
let allRecoveryReasons = [
    "legacy_execution_pending", "restore_journal_pending", "restore_journal_unreadable",
    "reconciliation_pending", "reconciliation_unreadable", "legacy_call_mode_lock",
    "legacy_adb_interface_lock", "legacy_lock_provenance_pending", "module_backup_present",
    "legacy_voice_runtime", "managed_voice_control_pending", "invalid_pristine_marker",
    "unreadable_evidence", "lifetime_authority_changed"
]
func crowdedFixture() throws -> Data {
    try fixture(Array(1...40).map(UInt64.init)) { value in
        value["events"] = Array(1...40).map { sequence -> [String: Any] in
            var row = record(UInt64(sequence))
            var event = row["event"] as! [String: Any]
            event["status"] = 409
            event["code"] = "http_client_error"
            row["event"] = event
            return row
        }
        value["warnings"] = ["live_current_runtime_only", "best_effort_not_lossless", "snapshot_not_atomic",
                             "live_window_truncated", "live_records_missed", "records_dropped",
                             "write_failed", "writer_unavailable"]
        value["module_recovery"] = [
            "schema_version": 1, "observation": "last_observed", "state": "legacy_recovery_required",
            "reason_codes": allRecoveryReasons, "historical_recovery_confirmed": false,
            "startup": ["metadata": "failed", "reconciliation": "publication_unconfirmed",
                        "native_inspection": "failed", "cleanup": "unconfirmed"]
        ]
    }
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
        let currentFailure = try LiveDiagnosticSnapshot.decode(fixture([]) { value in
            value["module_recovery"] = [
                "schema_version": 1, "observation": "last_observed", "state": "legacy_recovery_required",
                "reason_codes": allRecoveryReasons, "historical_recovery_confirmed": false,
                "startup": ["metadata": "inspected", "reconciliation": "not_applicable",
                            "native_inspection": "failed", "cleanup": "confirmed",
                            "legacy_current": "legacy_current_helper_unconfirmed"]
            ]
        })
        let currentSummary = currentFailure.recovery?.components(separatedBy: " · ").prefix(2).joined(separator: " · ") ?? ""
        try expect(currentFailure.rows.isEmpty && currentSummary.contains("legacy_current_helper_unconfirmed")
            && currentSummary.contains("无法确认模块语音服务空闲"),
            "retained failure was lost with an empty ring or hidden behind historical reason codes")
        let notices = try LiveDiagnosticSnapshot.decode(fixture { value in
            value["warnings"] = ["live_window_truncated", "live_records_missed", "synthetic-secret"]
        }).notices.joined()
        try expect(notices.contains("移出") && notices.contains("未能入队") && !notices.contains("synthetic"),
                   "live retention/missed notices were lost or copied unknown warning")
        print("PASS: memory-only client window, closed live DTO, byte/record/runtime bounds and dedup")

        let api = ControlledAPI()
        var failureClient = local
        let store = LiveDiagnosticStore(api: api, client: { failureClient }, interval: 10_000_000)
        store.setLifecycle(visible: true, running: true, instance: 1)
        try await waitUntil { await api.count() == 1 }
        // PID changes while state still reads running; do not overlap old/new requests.
        store.setLifecycle(visible: true, running: true, instance: 2)
        try expect(store.runtimeID == nil && store.updatedAt == nil, "old backend observation survived instance change")
        await api.complete(0, try fixture([100]))
        try await waitUntil { await api.count() == 2 }
        try expect(store.rows.allSatisfy { $0.id.hasPrefix("client:") }, "old response published into replacement instance")
        await api.complete(1, try fixture(Array(1...256).map(UInt64.init), runtime: secondRuntime))
        try await waitUntil { store.runtimeID == secondRuntime && store.rows.count == 500 }
        try expect(Set(store.rows.map(\.id)).count == store.rows.count, "merged list has duplicate IDs")
        try expect(zip(store.rows, store.rows.dropFirst()).allSatisfy { pair in pair.0.timeMS <= pair.1.timeMS }, "unordered backend response was not sorted")
        try await waitUntil { await api.count() == 3 }
        failureClient = ClientDiagnosticLiveSnapshot(session: local.session, records: local.records,
            evicted: local.evicted, dropped: 2, writeFailures: 3)
        await api.fail(2)
        try await waitUntil { store.updatedAt == nil && store.status.contains("读取失败") }
        try expect(!store.status.contains("raw-secret") && store.rows.allSatisfy { $0.id.hasPrefix("client:") },
                   "read failure leaked raw error or retained backend data as current")
        try expect(store.notices.contains("客户端较早记录已移出实时窗口。")
            && store.notices.contains("客户端持久日志有记录丢弃。")
            && store.notices.contains("客户端持久日志写入失败；实时记录不代表已落盘。"),
            "backend read failure cleared fresh client retention or persistence warnings")
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

        // A visible, finite ordinary row proves the default filter. Keep this
        // separate from the large LazyVStack whose later client rows are offscreen.
        let ordinaryClient = ClientDiagnostics()
        ordinaryClient.lifecycle(component: "application", stage: "started")
        let ordinary = ordinaryClient.liveSnapshot()
        let defaultAPI = ControlledAPI()
        let defaultStore = LiveDiagnosticStore(api: defaultAPI, client: { ordinary },
                                               interval: 60_000_000_000)
        let defaultBackend = BackendProcess()
        _ = NSApplication.shared
        NSApplication.shared.appearance = NSAppearance(named: .aqua)
        let defaultHost = NSHostingView(rootView: SupportDiagnosticsView(store: defaultStore)
            .environmentObject(defaultBackend).background(Color(nsColor: .windowBackgroundColor)))
        let defaultWindow = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 650),
                                     styleMask: [.titled], backing: .buffered, defer: false)
        defaultWindow.contentView = defaultHost
        defaultWindow.orderFront(nil)
        try await waitUntil { await defaultAPI.count() == 1 }
        await defaultAPI.complete(0, try fixture([]))
        try await waitUntil { defaultStore.updatedAt != nil }
        try await Task.sleep(nanoseconds: 50_000_000)
        defaultHost.layoutSubtreeIfNeeded()
        let defaultPNG = try saveScreenshot(defaultHost, named: "live-diagnostics-default-all.png")
        let defaultText = accessibilityText(in: defaultHost)
        let children = (defaultHost.accessibilityChildren() ?? []).map { String(describing: type(of: $0)) }
        let evidence = "DEFAULT AX children: \(children)\nDEFAULT AX text: \(String(defaultText.prefix(4096)))\n"
        FileHandle.standardError.write(Data(evidence.utf8))
        // Standalone NSHostingView can expose an empty AX tree on the runner.
        // Verify an actually rendered ordinary client lifecycle row using the native image
        // recognizer instead of inferring visibility from the store or AX.
        let recognize = VNRecognizeTextRequest()
        recognize.recognitionLevel = .accurate
        recognize.recognitionLanguages = ["zh-Hans", "en-US"]
        recognize.usesLanguageCorrection = false
        try VNImageRequestHandler(data: defaultPNG, options: [:]).perform([recognize])
        let renderedText = (recognize.results ?? []).compactMap { $0.topCandidates(1).first?.string }
            .joined(separator: " ")
        FileHandle.standardError.write(Data(("DEFAULT rendered OCR: " + String(renderedText.prefix(4096)) + "\n").utf8))
        let compactText = renderedText.filter { !$0.isWhitespace }.uppercased()
        try expect(compactText.contains("客户端") && compactText.contains("已启动"),
                   "default all-records screenshot hid an onscreen ordinary client lifecycle event")
        defaultWindow.contentView = nil
        defaultWindow.orderOut(nil)
        try await waitUntil { defaultStore.status == "实时读取已暂停。" }
        print("PASS: default all-records screenshot visibly includes an ordinary client lifecycle event")

        let uiAPI = ControlledAPI()
        let uiStore = LiveDiagnosticStore(api: uiAPI, client: { local }, interval: 10_000_000)
        let backend = BackendProcess()
        _ = NSApplication.shared
        let host = NSHostingView(rootView: SupportDiagnosticsView(store: uiStore).environmentObject(backend)
            .background(Color(nsColor: .windowBackgroundColor)))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 650),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        window.orderFront(nil)
        host.layoutSubtreeIfNeeded()
        try await waitUntil { await uiAPI.count() == 1 }
        await uiAPI.complete(0, try crowdedFixture())
        try await waitUntil { uiStore.updatedAt != nil }
        try expect(allRecoveryReasons.allSatisfy { uiStore.recovery?.contains($0) == true }
            && uiStore.notices.count >= 5, "crowded layout omitted recovery reasons or retention notices")
        try await Task.sleep(nanoseconds: 50_000_000)
        host.layoutSubtreeIfNeeded()
        try saveScreenshot(host, named: "live-diagnostics-900x650.png")
        try expect(abs(host.bounds.width - 900) <= 1 && abs(host.bounds.height - 650) <= 1,
                   "layout fixture did not retain the requested 900x650 viewport")
        guard let scroll = scrollViews(in: host).max(by: { $0.bounds.height < $1.bounds.height }) else {
            throw Failure.failed("actual hosted log scroll view was not found")
        }
        let viewport = host.convert(scroll.bounds, from: scroll)
        try expect(scroll.contentView.bounds.height >= 180, "crowded diagnostics collapsed the log viewport")
        try expect(viewport.minX >= -1 && viewport.minY >= -1
            && viewport.maxX <= host.bounds.maxX + 1 && viewport.maxY <= host.bounds.maxY + 1,
            "crowded diagnostics overflowed the 900x650 host")
        print("PASS: all 14 reasons and all notices at 900x650; log viewport \(scroll.contentView.bounds.height) px; synthetic screenshot written")
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
