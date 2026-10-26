import Foundation
import CoreFoundation

enum SupportBundleBuilder {
    static func make(client: ClientDiagnosticSnapshot, backend: Data?,
                     appVersion: String, build: String) throws -> Data {
        // Revalidate at the export boundary, even for an in-memory snapshot.
        let safeClient = ClientDiagnosticSnapshot(dropped: client.dropped, writeFailures: client.writeFailures,
            invalidRecords: client.invalidRecords,
            events: Array(client.events.filter { $0.validated() }.suffix(4096)),
            warnings: client.warnings.filter { clientWarnings.contains($0) })
        let clientObject = try JSONSerialization.jsonObject(with: JSONEncoder().encode(safeClient))
        var warnings = safeClient.warnings
        var runtime: Any = NSNull()
        if let backend, backend.count <= 2 * 1024 * 1024,
           let object = (try? JSONSerialization.jsonObject(with: backend)) as? [String: Any],
           let clean = sanitizeBackend(object) {
            runtime = clean
        } else {
            warnings.append("backend_snapshot_unavailable")
        }
        let package: [String: Any] = [
            "schema_version": 1, "kind": "switch-support-bundle",
            "generated_at_ms": Int64(Date().timeIntervalSince1970 * 1000),
            "app_version": version(appVersion), "build": buildNumber(build),
            "privacy": "technical_metadata_only_no_message_bodies_credentials_or_subscriber_ids",
            "client": clientObject, "backend": runtime,
            "warnings": warnings
        ]
        let data = try JSONSerialization.data(withJSONObject: package, options: [.prettyPrinted, .sortedKeys])
        guard data.count <= 8 * 1024 * 1024 else { throw CocoaError(.fileWriteOutOfSpace) }
        return data
    }

    private static let clientWarnings: Set<String> = [
        "bounded_retention", "non_atomic_snapshot", "counters_current_gui_session",
        "client_log_read_failed", "client_log_not_started", "client_queue_dropped_events",
        "client_log_write_failed", "client_invalid_records_skipped", "client_export_truncated"
    ]
    private static let runtimeWarnings: Set<String> = [
        "best_effort_not_lossless", "pending_queue_not_flushed", "snapshot_not_atomic",
        "current_runtime_counters", "export_truncated", "invalid_records_skipped",
        "read_failed", "records_dropped", "write_failed", "writer_unavailable"
    ]
    private static func version(_ value: String) -> String {
        !value.isEmpty && value.count <= 32
            && value.utf8.allSatisfy { (48...57).contains($0) || $0 == 46 } ? value : "unknown"
    }
    private static func buildNumber(_ value: String) -> String {
        !value.isEmpty && value.count <= 16
            && value.utf8.allSatisfy { (48...57).contains($0) } ? value : "unknown"
    }
    private static func hex(_ value: Any?, count: Int) -> String? {
        guard let value = value as? String, value.utf8.count == count,
              value.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else { return nil }
        return value
    }
    private static func number(_ value: Any?) -> NSNumber? {
        guard let value = value as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID(),
              UInt64(value.stringValue) != nil else { return nil }
        return value
    }
    private static func numbers(_ source: Any?, keys: [String]) -> [String: Any]? {
        guard let source = source as? [String: Any] else { return nil }
        var result: [String: Any] = [:]
        for key in keys {
            guard let value = number(source[key]) else { return nil }
            result[key] = value
        }
        return result
    }
    private static func sanitizeBackend(_ object: [String: Any]) -> [String: Any]? {
        guard number(object["schema_version"])?.uint64Value == 1,
              let generated = number(object["generated_at_ms"]),
              let runtimeID = hex(object["runtime_id"], count: 32),
              let limits = numbers(object["limits"], keys: ["queue_capacity","max_record_bytes","segment_bytes","segments","export_bytes","max_events"]),
              let counters = numbers(object["counters"], keys: ["accepted","written","dropped","write_failures","invalid_records","read_failures"]),
              let records = object["events"] as? [Any], records.count <= 4096,
              let sourceWarnings = object["warnings"] as? [String], sourceWarnings.count <= 32 else { return nil }
        let events = records.compactMap { sanitizeRecord($0) }
        var warnings = sourceWarnings.filter { runtimeWarnings.contains($0) }
        if events.count != records.count { warnings.append("backend_invalid_records_skipped") }
        return [
            "schema_version": 1, "generated_at_ms": generated, "runtime_id": runtimeID,
            "version": version(object["version"] as? String ?? ""),
            "build": buildNumber(object["build"] as? String ?? ""),
            "source_digest": hex(object["source_digest"], count: 64) ?? "unknown",
            "limits": limits, "counters": counters, "events": events, "warnings": warnings
        ]
    }
    private static func sanitizeRecord(_ input: Any) -> [String: Any]? {
        guard let object = input as? [String: Any],
              number(object["schema_version"])?.uint64Value == 1,
              let runtimeID = hex(object["runtime_id"], count: 32),
              let sequence = number(object["sequence"]), sequence.uint64Value > 0,
              let time = number(object["time_ms"]),
              let event = sanitizeEvent(object["event"]) else { return nil }
        return ["schema_version": 1, "runtime_id": runtimeID, "sequence": sequence,
                "time_ms": time, "event": event]
    }
    private static func sanitizeEvent(_ input: Any?) -> [String: Any]? {
        guard let object = input as? [String: Any], let kind = object["kind"] as? String else { return nil }
        switch kind {
        case "lifecycle":
            guard let phase = object["phase"] as? String,
                  ["startup","shutdown_started","shutdown_complete","shutdown_failed"].contains(phase) else { return nil }
            return ["kind": kind, "phase": phase]
        case "api":
            guard let route = object["route"] as? String,
                  route == "unmatched" || (route.hasPrefix("/api/") && ClientDiagnosticEvent.routes.contains(route)),
                  let method = object["method"] as? String,
                  ["GET","HEAD","POST","PUT","PATCH","DELETE","OPTIONS","OTHER"].contains(method),
                  let status = number(object["status"]), status.uint64Value == 0 || (100...599).contains(status.uint64Value),
                  let duration = number(object["duration_ms"]),
                  let correlation = hex(object["correlation_id"], count: 32) else { return nil }
            let code: String
            switch status.uint64Value {
            case 0: code = "request_cancelled"
            case 100...399: code = "http_success"
            case 400...499: code = "http_client_error"
            default: code = "http_server_error"
            }
            return ["kind": kind, "route": route, "method": method, "status": status,
                    "code": code, "duration_ms": duration, "correlation_id": correlation]
        case "safe_event":
            guard let detail = object["detail"] as? [String: Any],
                  let values = numbers(detail, keys: ["sequence","time_ms","generation","duration_ms"]),
                  let correlation = hex(detail["correlation_id"], count: 32),
                  let operation = detail["operation"] as? String,
                  ["ims_register","nwait_session","network_diagnostic","reader_auth","sms_send","sms_forward"].contains(operation),
                  let stage = detail["stage"] as? String,
                  ["queued","waiting_lock","prepare","connect","authenticate","ready","submit","retire","complete"].contains(stage),
                  let code = detail["code"] as? String,
                  ["started","ready","planned_renewal","planned_ike_reauth","manual_retry","success","cancelled",
                   "busy","timeout","transport_failed","authentication_failed","registration_rejected","binding_revoked",
                   "binding_unconfirmed","binding_ambiguous","submission_failed","result_unknown","stopped",
                   "unavailable","invalid_response"].contains(code),
                  let result = detail["result"] as? String,
                  ["pending","succeeded","failed","cancelled","unknown"].contains(result) else { return nil }
            var sanitized = values
            sanitized["correlation_id"] = correlation
            sanitized["operation"] = operation
            sanitized["stage"] = stage
            sanitized["code"] = code
            sanitized["result"] = result
            return ["kind": kind, "detail": sanitized]
        default: return nil
        }
    }
}

/// Cancellation releases the view, not the filesystem owner. Keep one lease
/// across the awaited detached reader/writer until its actual work has retired.
final class SupportExportLease: @unchecked Sendable {
    private static let lock = NSLock()
    private static var busy = false
    private var released = false
    private init() {}
    static func acquire() -> SupportExportLease? {
        lock.lock()
        defer { lock.unlock() }
        guard !busy else { return nil }
        busy = true
        return SupportExportLease()
    }
    func release() {
        Self.lock.lock()
        defer { Self.lock.unlock() }
        guard !released else { return }
        released = true
        Self.busy = false
    }
    deinit { release() }
}
