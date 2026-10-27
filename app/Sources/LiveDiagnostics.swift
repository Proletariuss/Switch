import Combine
import CoreFoundation
import Foundation

struct LiveDiagnosticRow: Identifiable, Equatable {
    let id: String
    let timeMS: UInt64
    let source: String
    let title: String
    let detail: String
    let warning: Bool
    var date: Date { Date(timeIntervalSince1970: Double(timeMS) / 1000) }
}

struct LiveDiagnosticSnapshot {
    let runtimeID: String
    let rows: [LiveDiagnosticRow]
    let recovery: String?
    let notices: [String]

    private static func number(_ value: Any?) -> UInt64? {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        return UInt64(number.stringValue)
    }

    static func decode(_ data: Data) throws -> Self {
        guard data.count <= 294_912,
              let raw = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              raw["view"] as? String == "live",
              let limits = raw["limits"] as? [String: Any],
              number(limits["max_events"]) == 256,
              number(limits["max_record_bytes"]) == 4096,
              number(limits["memory_bytes"]) == 262_144,
              number(limits["response_bytes"]) == 294_912,
              let records = raw["events"] as? [Any], records.count <= 256,
              let counters = raw["counters"] as? [String: Any],
              let evicted = number(counters["live_evicted"]),
              let missed = number(counters["live_missed"]),
              let retained = number(counters["retained_bytes"]), retained <= 262_144,
              let warnings = raw["warnings"] as? [String], warnings.count <= 32 else {
            throw Failure.invalid
        }
        var recordBytes = 0
        for item in records {
            guard let record = item as? [String: Any],
                  JSONSerialization.isValidJSONObject(record) else { throw Failure.invalid }
            let bytes = try JSONSerialization.data(withJSONObject: record)
            guard bytes.count <= 4096 else { throw Failure.invalid }
            recordBytes += bytes.count
        }
        guard recordBytes <= 262_144 else { throw Failure.invalid }
        // Reuse the export boundary's closed event/recovery vocabulary entirely
        // in memory. Extra fields and invalid records never reach rendered text.
        let empty = ClientDiagnosticSnapshot(dropped: 0, writeFailures: 0, invalidRecords: 0,
                                             events: [], warnings: [])
        let safeData = try SupportBundleBuilder.make(client: empty, backend: data,
                                                     appVersion: "0", build: "0")
        guard let package = try JSONSerialization.jsonObject(with: safeData) as? [String: Any],
              let safe = package["backend"] as? [String: Any],
              let runtimeID = safe["runtime_id"] as? String,
              let events = safe["events"] as? [[String: Any]] else { throw Failure.invalid }
        var seen = Set<String>()
        var rows: [LiveDiagnosticRow] = []
        for event in events {
            guard event["runtime_id"] as? String == runtimeID,
                  let sequence = number(event["sequence"]),
                  let time = number(event["time_ms"]), time <= 253_402_300_799_999,
                  let body = event["event"] as? [String: Any] else { throw Failure.invalid }
            let id = "backend:\(runtimeID):\(sequence)"
            guard seen.insert(id).inserted else { continue }
            guard let row = backendRow(id: id, time: time, body: body) else { continue }
            rows.append(row)
        }
        var notices: [String] = []
        if evicted > 0 || warnings.contains("live_window_truncated") {
            notices.append("后台较早记录已移出实时窗口。")
        }
        if missed > 0 || warnings.contains("live_records_missed") {
            notices.append("部分后台实时记录未能入队；列表并非完整审计记录。")
        }
        if events.count != records.count { notices.append("已跳过无法确认的后台记录。") }
        if warnings.contains("records_dropped") { notices.append("后台持久日志有记录丢弃。") }
        if warnings.contains("write_failed") || warnings.contains("writer_unavailable") {
            notices.append("后台持久日志写入未确认；实时记录不代表已落盘。")
        }
        var recovery: String?
        if let value = safe["module_recovery"] as? [String: Any],
           let state = value["state"] as? String,
           let reasons = value["reason_codes"] as? [String],
           let startup = value["startup"] as? [String: Any] {
            let label = state == "legacy_recovery_required" ? "模块已隔离" :
                (state == "open" ? "最近观察到准入开放" : "尚无恢复状态观察")
            recovery = "最近观察：\(label)"
            if !reasons.isEmpty {
                let labels = ["legacy_execution_pending": "旧版执行尚待确认",
                              "restore_journal_pending": "还原记录尚待确认",
                              "restore_journal_unreadable": "还原记录无法读取",
                              "reconciliation_pending": "恢复核对尚未完成",
                              "reconciliation_unreadable": "恢复核对记录无法读取",
                              "legacy_call_mode_lock": "旧通话模式锁待核对",
                              "legacy_adb_interface_lock": "旧 ADB 接口锁待核对",
                              "legacy_lock_provenance_pending": "旧锁来源待核对",
                              "module_backup_present": "存在模块备份待核对",
                              "legacy_voice_runtime": "旧语音运行时待核对",
                              "managed_voice_control_pending": "语音控制清理待核对",
                              "invalid_pristine_marker": "初始状态标记无效",
                              "unreadable_evidence": "恢复证据无法读取",
                              "lifetime_authority_changed": "后台目录租约已变化"]
                recovery! += " · " + reasons.map { "\(labels[$0] ?? $0) [\($0)]" }.joined(separator: " · ")
            }
            if let phase = startup["reconciliation"] as? String,
               let cleanup = startup["cleanup"] as? String {
                let labels = ["not_observed": "未观察", "not_needed": "无需检查", "inspected": "已检查",
                              "failed": "失败", "not_applicable": "不适用", "completed": "已完成",
                              "completed_after_stop": "停止后完成", "publication_unconfirmed": "发布未确认",
                              "confirmed": "已确认", "unconfirmed": "未确认"]
                let metadata = startup["metadata"] as? String ?? "not_observed"
                let native = startup["native_inspection"] as? String ?? "not_observed"
                recovery! += " · 元数据 \(labels[metadata] ?? metadata)"
                    + " · 恢复核对 \(labels[phase] ?? phase)"
                    + " · 原生检查 \(labels[native] ?? native)"
                    + " · 清理 \(labels[cleanup] ?? cleanup)"
            }
        } else if raw["module_recovery"] != nil {
            notices.append("恢复状态无法确认。")
        }
        return Self(runtimeID: runtimeID, rows: rows, recovery: recovery, notices: notices)
    }

    private static func backendRow(id: String, time: UInt64, body: [String: Any]) -> LiveDiagnosticRow? {
        guard let kind = body["kind"] as? String else { return nil }
        switch kind {
        case "lifecycle":
            guard let phase = body["phase"] as? String else { return nil }
            let labels = ["startup": "后台启动", "shutdown_started": "后台开始退出",
                          "shutdown_complete": "后台退出完成", "shutdown_failed": "后台退出未确认"]
            return .init(id: id, timeMS: time, source: "后台", title: labels[phase] ?? "后台状态",
                         detail: phase, warning: phase == "shutdown_failed")
        case "api":
            guard let method = body["method"] as? String, let route = body["route"] as? String,
                  let status = number(body["status"]), let duration = number(body["duration_ms"]),
                  let correlation = body["correlation_id"] as? String else { return nil }
            return .init(id: id, timeMS: time, source: "后台请求", title: "\(method) \(route)",
                         detail: "\(httpOutcome(status)) · HTTP \(status) · \(duration) ms · 关联 \(correlation.prefix(12))",
                         warning: status == 0 || status >= 400)
        case "safe_event":
            guard let detail = body["detail"] as? [String: Any],
                  let op = detail["operation"] as? String, let stage = detail["stage"] as? String,
                  let code = detail["code"] as? String, let result = detail["result"] as? String,
                  let duration = number(detail["duration_ms"]),
                  let correlation = detail["correlation_id"] as? String else { return nil }
            return .init(id: id, timeMS: time, source: "操作事件",
                title: "\(DiagnosticOperation(rawValue: op)?.label ?? op) · \(DiagnosticStage(rawValue: stage)?.label ?? stage)",
                detail: "\(DiagnosticEventResult(rawValue: result)?.label ?? result) · \(DiagnosticEventCode(rawValue: code)?.label ?? code) · \(duration) ms · 关联 \(correlation.prefix(12))",
                warning: result == "failed" || result == "unknown")
        case "module":
            guard let detail = body["detail"] as? [String: Any],
                  let stage = detail["stage"] as? String, let result = detail["result"] as? String,
                  let code = detail["code"] as? String, let transport = detail["transport"] as? String,
                  let duration = number(detail["duration_ms"]),
                  let repeats = number(detail["suppressed_repeats"]) else { return nil }
            return .init(id: id, timeMS: time, source: "模块", title: "\(stage) · \(result)",
                         detail: "\(code) · \(transport) · \(duration) ms · 合并重复 \(repeats)",
                         warning: result == "failed" || result == "deferred")
        default: return nil
        }
    }

    private static func httpOutcome(_ status: UInt64) -> String {
        switch status {
        case 0: return "请求尚未返回或等待已结束"
        case 100...399: return "请求已返回"
        case 401, 403: return "请求未获授权"
        case 409: return "请求被拒绝或资源占用，请结合上方恢复状态核对"
        case 408, 504: return "请求超时"
        case 503: return "服务暂不可用"
        default: return "请求未完成"
        }
    }

    static func clientRows(_ snapshot: ClientDiagnosticLiveSnapshot) -> [LiveDiagnosticRow] {
        snapshot.records.suffix(256).compactMap { record in
            let event = record.event
            guard event.validated(), event.timeMS <= 253_402_300_799_999 else { return nil }
            if event.method == "NONE", ["application", "backend"].contains(event.route) {
                let labels = ["started": "已启动", "starting": "启动中", "running": "运行中",
                              "stopping": "正在退出", "stopped": "已停止", "failed": "运行失败"]
                let component = event.route == "application" ? "客户端" : "后台进程"
                return LiveDiagnosticRow(id: "client:\(snapshot.session):\(record.sequence)",
                    timeMS: UInt64(event.timeMS), source: "客户端生命周期",
                    title: "\(component) · \(labels[event.stage] ?? "状态已更新")",
                    detail: "本次客户端会话记录 · 关联 \(event.correlationID.prefix(12))",
                    warning: event.stage == "failed")
            }
            return LiveDiagnosticRow(id: "client:\(snapshot.session):\(record.sequence)",
                timeMS: UInt64(event.timeMS), source: "客户端",
                title: "\(event.method == "NONE" ? "" : event.method + " ")\(event.route) · \(event.stage)",
                detail: "\(httpOutcome(UInt64(event.status))) · HTTP \(event.status) · \(event.durationMS) ms · 关联 \(event.correlationID.prefix(12))"
                    + (event.errorDomain == "url" ? " · URL \(event.errorCode)" : ""),
                warning: event.stage == "failed" || event.stage == "transport_failed" || event.status >= 400)
        }
    }

    enum Failure: Error { case invalid }
}

protocol LiveDiagnosticAPI {
    func snapshot() async throws -> Data
}
private struct NativeLiveDiagnosticAPI: LiveDiagnosticAPI {
    func snapshot() async throws -> Data {
        try await APIClient(timeoutInterval: 6).getData("api/diagnostics/support?view=live")
    }
}

/// One finite request owner across hide/restart/cancel races. A replacement
/// poll waits until the cancelled old request actually returns.
@MainActor final class LiveDiagnosticStore: ObservableObject {
    @Published private(set) var rows: [LiveDiagnosticRow] = []
    @Published private(set) var status = "等待读取实时记录。"
    @Published private(set) var recovery: String?
    @Published private(set) var notices: [String] = []
    @Published private(set) var updatedAt: Date?
    @Published private(set) var runtimeID: String?
    private let api: any LiveDiagnosticAPI
    private let client: () -> ClientDiagnosticLiveSnapshot
    private let interval: UInt64
    private var visible = false
    private var running = false
    private var instance: Int32 = 0
    private var revision: UInt64 = 0
    private var worker: Task<Void, Never>?
    private var backendRows: [LiveDiagnosticRow] = []

    init(api: (any LiveDiagnosticAPI)? = nil,
         client: @escaping () -> ClientDiagnosticLiveSnapshot = { ClientDiagnostics.shared.liveSnapshot() },
         interval: UInt64 = 2_000_000_000) {
        self.api = api ?? NativeLiveDiagnosticAPI()
        self.client = client
        self.interval = interval
    }
    deinit { worker?.cancel() }

    func setLifecycle(visible: Bool, running: Bool, instance: Int32 = 0) {
        guard self.visible != visible || self.running != running || self.instance != instance else { return }
        self.visible = visible
        self.running = running
        self.instance = instance
        revision &+= 1
        worker?.cancel()
        backendRows = []
        runtimeID = nil
        recovery = nil
        updatedAt = nil
        notices = []
        merge(client())
        status = !visible ? "实时读取已暂停。" :
            (running ? "正在读取当前后台记录…" : "后端未运行，仅显示当前客户端记录。")
        launchIfNeeded()
    }

    private func launchIfNeeded() {
        guard visible, worker == nil else { return }
        let captured = revision
        let delay = interval
        worker = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                // Optional chaining retains the owner only for this finite poll.
                // No strong self binding survives into the sleep or next iteration.
                guard await self?.pollIfCurrent(captured) == true else { break }
                do { try await Task.sleep(nanoseconds: delay) } catch { break }
            }
            self?.worker = nil
            self?.launchIfNeeded()
        }
    }

    private func pollIfCurrent(_ captured: UInt64) async -> Bool {
        guard !Task.isCancelled, visible, revision == captured else { return false }
        await poll(captured)
        return !Task.isCancelled && visible && revision == captured
    }

    private func poll(_ captured: UInt64) async {
        let local = client()
        merge(local)
        guard running else {
            notices = clientNotices(local)
            return
        }
        do {
            let data = try await api.snapshot()
            guard !Task.isCancelled, visible, running, revision == captured else { return }
            let snapshot = try await Task.detached(priority: .utility) {
                try LiveDiagnosticSnapshot.decode(data)
            }.value
            guard !Task.isCancelled, visible, running, revision == captured else { return }
            let changed = runtimeID != nil && runtimeID != snapshot.runtimeID
            runtimeID = snapshot.runtimeID
            backendRows = snapshot.rows
            recovery = snapshot.recovery
            notices = snapshot.notices
            notices += clientNotices(local)
            merge(client())
            updatedAt = Date()
            status = changed ? "后台实例已切换，当前列表已更新。" : "实时读取中 · 每 2 秒刷新"
        } catch {
            guard !Task.isCancelled, visible, running, revision == captured else { return }
            backendRows = []
            recovery = nil
            updatedAt = nil
            notices = []
            merge(client())
            status = "后台实时记录读取失败，当前后台状态未知；正在自动重试。"
        }
    }

    private func clientNotices(_ snapshot: ClientDiagnosticLiveSnapshot) -> [String] {
        var result: [String] = []
        if snapshot.evicted > 0 { result.append("客户端较早记录已移出实时窗口。") }
        if snapshot.dropped > 0 { result.append("客户端持久日志有记录丢弃。") }
        if snapshot.writeFailures > 0 { result.append("客户端持久日志写入失败；实时记录不代表已落盘。") }
        return result
    }

    private func merge(_ local: ClientDiagnosticLiveSnapshot) {
        let combined = backendRows + LiveDiagnosticSnapshot.clientRows(local)
        var seen = Set<String>()
        rows = Array(combined.sorted {
            $0.timeMS == $1.timeMS ? $0.id < $1.id : $0.timeMS < $1.timeMS
        }.filter { seen.insert($0.id).inserted }.suffix(500))
    }
}
