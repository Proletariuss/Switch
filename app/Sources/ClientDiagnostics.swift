import Foundation

/// Typed technical facts only; no free-form errors, bodies, credentials or subscriber IDs.
struct ClientDiagnosticEvent: Codable {
    let schemaVersion: Int
    let timeMS: Int64
    let correlationID: String
    let requestID: String?
    let route: String
    let method: String
    let stage: String
    let status: Int
    let errorDomain: String
    let errorCode: Int
    let durationMS: Int64
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", timeMS = "time_ms", correlationID = "correlation_id"
        case requestID = "request_id", route, method, stage, status
        case errorDomain = "error_domain", errorCode = "error_code", durationMS = "duration_ms"
    }
    static let routes: Set<String> = [
        "/api/at",
        "/api/call-mode/backups",
        "/api/call-mode/backups/delete",
        "/api/call-mode/backups/export",
        "/api/call-mode/backups/import",
        "/api/call-mode/status",
        "/api/call-mode/download",
        "/api/call-mode/enable",
        "/api/call-mode/restore",
        "/api/call-mode/retry",
        "/api/vowifi",
        "/api/vowifi/reconcile",
        "/api/vowifi/stop-control",
        "/api/reader/vowifi/ussd",
        "/api/vowifi/calls/dial",
        "/api/vowifi/calls/answer",
        "/api/vowifi/calls/reject",
        "/api/vowifi/calls/hangup",
        "/api/vowifi/calls/dtmf",
        "/api/reader/vowifi/calls/dial",
        "/api/reader/vowifi/calls/answer",
        "/api/reader/vowifi/calls/reject",
        "/api/reader/vowifi/calls/hangup",
        "/api/reader/vowifi/calls/dtmf",
        "/api/vowifi/result",
        "/api/reader/vowifi",
        "/api/reader/vowifi/reconcile",
        "/api/reader/vowifi/stop-control",
        "/api/reader/vowifi/sessions",
        "/api/reader/vowifi/calls/status",
        "/api/vowifi/calls/status",
        "/api/call/answer",
        "/api/call/audio/start",
        "/api/call/audio/stop",
        "/api/call/dial",
        "/api/call/dtmf",
        "/api/call/hangup",
        "/api/call/status",
        "/api/calls",
        "/api/calls/clear",
        "/api/calls/delete",
        "/api/diagnostics/events",
        "/api/diagnostics/support",
        "/api/esim/notes",
        "/api/esim/module-notes",
        "/api/esim",
        "/api/esim/mutation-intent",
        "/api/esim/download",
        "/api/esim/download/confirm",
        "/api/esim/health",
        "/api/esim/phonebook/probe",
        "/api/esim/profile",
        "/api/esim/switch",
        "/api/health",
        "/api/status",
        "/api/module/flight-mode",
        "/api/multistandby/changes",
        "/api/multistandby/conversations",
        "/api/multistandby/cards",
        "/api/multistandby/control-intents",
        "/api/multistandby/control-intents/ack",
        "/api/multistandby/history",
        "/api/multistandby/messages",
        "/api/multistandby/messages/send",
        "/api/multistandby/messages/{message_id}",
        "/api/multistandby/operations/{id}",
        "/api/multistandby/profiles",
        "/api/multistandby/report",
        "/api/multistandby/retry",
        "/api/multistandby/start",
        "/api/multistandby/stop",
        "/api/multistandby/send-intents",
        "/api/multistandby/send-intents/submit",
        "/api/network/check-4g",
        "/api/network",
        "/api/network/reboot-module",
        "/api/network/recovery",
        "/api/network/services",
        "/api/network/services-order",
        "/api/network/traffic",
        "/api/network/usbnet",
        "/api/reader/ble/status",
        "/api/reader/ble/scan",
        "/api/reader/ble/connect",
        "/api/reader/ble/disconnect",
        "/api/reader/auth",
        "/api/reader/connect",
        "/api/reader/disconnect",
        "/api/reader/refresh",
        "/api/reader/esim",
        "/api/reader/esim/mutation-intent",
        "/api/reader/esim/health",
        "/api/reader/esim/download",
        "/api/reader/esim/download/confirm",
        "/api/reader/esim/notifications",
        "/api/reader/esim/notifications/retry",
        "/api/reader/esim/operation",
        "/api/esim/operation",
        "/api/esim/operation/acknowledge",
        "/api/reader/imei",
        "/api/reader/esim/profile",
        "/api/reader/esim/switch",
        "/api/reader/info",
        "/api/reader/list",
        "/api/reader/vowifi/proxy-check",
        "/api/sms",
        "/api/sms/devices",
        "/api/sms/adopt",
        "/api/sms/clear",
        "/api/sms/delete",
        "/api/sms/delete-sender",
        "/api/sms/refresh",
        "/api/sms/storage",
        "/api/sms/forwarding/configure",
        "/api/sms/forwarding/device-snapshot",
        "/api/sms/forwarding/enqueue",
        "/api/sms/forwarding/queue-action",
        "/api/sms/forwarding/status",
        "/api/sms/forwarding/test",
        "/api/sms/send",
        "/api/sms/send-intents",
        "/api/sms/status",
        "/api/v2/call/dtmf",
        "/api/v2/call/negotiate-sdp",
        "/api/v2/call/build-local-sdp",
        "/api/v2/call/invite-policy",
        "/api/v2/carrier/resolve",
        "/api/v2/device-snapshot",
        "/api/v2/ims/validate-register-response",
        "/api/v2/migration-readiness",
        "/api/v2/reader/authenticate",
        "/api/v2/reader/eap-aka",
        "/api/v2/reader/identity",
        "/api/v2/runtime",
        "/api/v2/sms-archive",
        "/api/v2/sms-archive/clear",
        "/api/v2/sms-archive/delete",
        "/api/v2/sms-archive/delete-sender",
        "/api/v2/sms-export",
        "/api/v2/sms-export/{token}",
        "/api/v2/sms/decode-incoming-tpdu",
        "/api/v2/sms/decode-rpdu",
        "/api/v2/sms/decode-stored-snapshot",
        "/api/v2/sms/encode-rpdu",
        "/api/v2/sms/forwarding/handoff",
        "/api/v2/sms/prepare-tpdu",
        "/api/v2/vowifi/eap-exchange/release",
        "/api/v2/vowifi/eap-identity",
        "/api/v2/vowifi/native-session-plan",
        "/api/v2/vowifi/proxy-policy",
        "/api/v2/vowifi/proxy-probe",
        "/api/v2/vowifi/select-transport",
        "/api/v2/vowifi/session-policy",
        "/api/voice/enable",
        "/api/voice/enabled",
        "/api/other",
        "application",
        "backend"
    ]
    func validated() -> Bool {
        schemaVersion == 1 && timeMS >= 0 && durationMS >= 0
        && correlationID.utf8.count == 32 && correlationID.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
        && (requestID == nil || UUID(uuidString: requestID!) != nil)
        && Self.routes.contains(route) && ["GET","POST","PUT","PATCH","DELETE","HEAD","OTHER","NONE"].contains(method)
        && ["started","response","transport_failed","stopped","starting","running","stopping","failed"].contains(stage)
        && (status == 0 || (100...599).contains(status)) && ["none","url"].contains(errorDomain)
        && (-99999...99999).contains(errorCode)
    }
}
struct ClientDiagnosticSnapshot: Codable {
    let schemaVersion = 1
    let segmentBytes = 1_048_576
    let segments = 3
    let queueCapacity = BoundedBLEDiagnosticLog.capacity
    let dropped: UInt64
    let writeFailures: UInt64
    let invalidRecords: Int
    let events: [ClientDiagnosticEvent]
    let warnings: [String]
}
final class ClientDiagnostics: @unchecked Sendable {
    static let shared = ClientDiagnostics()
    private let lock = NSLock()
    private var writer: BoundedBLEDiagnosticLog?
    private var observer: NSObjectProtocol?
    private var dropped: UInt64 = 0
    private var writeFailures: UInt64 = 0
    private let session = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
    init() {
        observer = NotificationCenter.default.addObserver(
            forName: Notification.Name("Switch.SafeAPIDiagnostic.v1"), object: nil, queue: nil
        ) { [weak self] notice in
            guard let fields = notice.userInfo,
                  JSONSerialization.isValidJSONObject(fields),
                  let data = try? JSONSerialization.data(withJSONObject: fields),
                  let event = try? JSONDecoder().decode(ClientDiagnosticEvent.self, from: data),
                  event.validated() else { return }
            self?.record(event)
        }
    }
    deinit { if let observer { NotificationCenter.default.removeObserver(observer) } }
    func start(root: URL) {
        lock.lock()
        if writer != nil { lock.unlock(); return }
        let directory = root.appendingPathComponent("client-diagnostics", isDirectory: true)
        writer = BoundedBLEDiagnosticLog(directory: { directory }, maximumFileBytes: 1_048_576,
            fileNames: ["client-events.jsonl", "client-events.jsonl.1", "client-events.jsonl.2"],
            onWriteFailure: { [weak self] in self?.failedWrite() })
        lock.unlock()
        lifecycle(component: "application", stage: "started")
    }
    private func failedWrite() { lock.lock(); writeFailures &+= 1; lock.unlock() }
    func lifecycle(component: String, stage: String) {
        record(ClientDiagnosticEvent(schemaVersion: 1, timeMS: Int64(Date().timeIntervalSince1970 * 1000),
            correlationID: session, requestID: nil, route: component, method: "NONE", stage: stage,
            status: 0, errorDomain: "none", errorCode: 0, durationMS: 0))
    }
    private func record(_ event: ClientDiagnosticEvent) {
        guard event.validated(), let encoded = try? JSONEncoder().encode(event) else { return }
        lock.lock(); let target = writer; lock.unlock()
        guard let target else { return }
        if !target.enqueue(String(decoding: encoded, as: UTF8.self)) {
            lock.lock(); dropped &+= 1; lock.unlock()
        }
    }
    /// Background-only bounded read, never flushes/joins a communication owner.
    func snapshot() -> ClientDiagnosticSnapshot {
        lock.lock(); let target = writer; let dropCount = dropped; let failureCount = writeFailures; lock.unlock()
        var events: [ClientDiagnosticEvent] = []
        var invalid = 0
        var warnings = ["bounded_retention", "non_atomic_snapshot", "counters_current_gui_session"]
        if let target {
            do {
                for segment in try target.snapshotSegments() {
                    for raw in segment.split(separator: 0x0a, omittingEmptySubsequences: true) {
                        guard let split = raw.firstIndex(of: 0x20),
                              let event = try? JSONDecoder().decode(ClientDiagnosticEvent.self, from: Data(raw[raw.index(after: split)...])),
                              event.validated() else { invalid += 1; continue }
                        events.append(event)
                    }
                }
            } catch { warnings.append("client_log_read_failed") }
        } else { warnings.append("client_log_not_started") }
        if dropCount > 0 { warnings.append("client_queue_dropped_events") }
        if failureCount > 0 { warnings.append("client_log_write_failed") }
        if invalid > 0 { warnings.append("client_invalid_records_skipped") }
        if events.count > 4096 { events = Array(events.suffix(4096)); warnings.append("client_export_truncated") }
        return ClientDiagnosticSnapshot(dropped: dropCount, writeFailures: failureCount,
            invalidRecords: invalid, events: events, warnings: warnings)
    }
}
