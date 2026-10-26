import Darwin
import Foundation

enum SwitchRuntimePaths {
    static func dataDirectory() -> URL { fatalError("test must inject its isolated root") }
}
private struct CheckFailure: Error, CustomStringConvertible { let description: String }
private func expect(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !condition() { throw CheckFailure(description: message) }
}

@main private enum SupportDiagnosticsTests {
    static let fm = FileManager.default
    static let names = ["client-events.jsonl", "client-events.jsonl.1", "client-events.jsonl.2"]
    static let limit = 1_048_576
    static let secret = "PRIVATE_FIXTURE_phone_15550009876_body_password_token_subscriber"
    static let root: URL = {
        let build = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"]!).resolvingSymlinksInPath()
        let target = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SUPPORT_DIAGNOSTIC_TEST_ROOT"]!)
            .standardizedFileURL.resolvingSymlinksInPath()
        precondition(target.path.hasPrefix(build.path + "/"))
        try! fm.createDirectory(at: target, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        return target
    }()
    static func directory(_ name: String) throws -> URL {
        let path = root.appendingPathComponent(name, isDirectory: true)
        try fm.createDirectory(at: path, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        return path
    }
    static func write(_ data: Data, to path: URL) throws {
        try data.write(to: path)
        try expect(chmod(path.path, 0o600) == 0, "fixture chmod failed")
    }
    static func event(_ time: Int = 1) -> [String: Any] {
        ["schema_version": 1, "time_ms": time, "correlation_id": String(repeating: "a", count: 32),
         "request_id": "7F3829A6-7CA1-4E52-88C1-70B867A28431", "route": "/api/sms/send",
         "method": "POST", "stage": "response", "status": 200,
         "error_domain": "none", "error_code": 0, "duration_ms": 1]
    }
    static func line(_ object: [String: Any]) throws -> Data {
        var data = Data("2000-01-01T00:00:00Z ".utf8)
        data.append(try JSONSerialization.data(withJSONObject: object))
        data.append(0x0a)
        return data
    }
    static func json(_ data: Data) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: data) as! [String: Any]
    }
    static func settled(_ diagnostics: ClientDiagnostics,
                        until condition: (ClientDiagnosticSnapshot) -> Bool) throws -> ClientDiagnosticSnapshot {
        for _ in 0..<500 {
            let value = diagnostics.snapshot()
            if condition(value) { return value }
            Thread.sleep(forTimeInterval: 0.01)
        }
        throw CheckFailure(description: "diagnostic writer did not settle")
    }
    static func emit(_ logger: BoundedBLEDiagnosticLog, _ value: String) throws {
        try expect(logger.enqueue(value), "isolated writer unexpectedly dropped event")
        logger.waitUntilIdleForTesting()
    }

    static func rotationAndRestart() throws {
        let path = try directory("rotation")
        let logger = BoundedBLEDiagnosticLog(directory: { path }, maximumFileBytes: limit, fileNames: names)
        try write(Data("retained-middle\n".utf8), to: path.appendingPathComponent(names[1]))
        try write(Data("expired-oldest\n".utf8), to: path.appendingPathComponent(names[2]))
        for index in 0..<512 {
            let prefix = "entry-\(index) "
            try emit(logger, prefix + String(repeating: "x", count: 2026 - prefix.utf8.count))
        }
        let current = path.appendingPathComponent(names[0])
        try expect(try Data(contentsOf: current).count == limit, "configured 1 MiB exact boundary changed")
        try emit(logger, "after-first-rotation")
        let before = try Data(contentsOf: current)
        let segments = try logger.snapshotSegments()
        try expect(segments.count == 3 && segments.allSatisfy { $0.count <= limit }, "segment bound exceeded")
        try expect(segments.reduce(0) { $0 + $1.count } <= 3 * limit, "3 MiB total bound exceeded")
        try expect(String(decoding: segments[0], as: UTF8.self) == "retained-middle\n", "rotation order changed")
        try expect(!segments.contains { String(decoding: $0, as: UTF8.self).contains("expired-oldest") }, "expired segment retained")
        let reopened = BoundedBLEDiagnosticLog(directory: { path }, maximumFileBytes: limit, fileNames: names)
        try expect(try reopened.snapshotSegments() == segments, "restart changed retained diagnostics")
        try emit(reopened, "after-restart")
        try expect(try Data(contentsOf: current).starts(with: before), "reopened writer truncated current segment")
        for name in names {
            var info = stat()
            try expect(lstat(path.appendingPathComponent(name).path, &info) == 0
                && BoundedBLEDiagnosticLog.isPrivateRegularFile(info), "private ownership/mode lost")
        }
        print("PASS support storage: production 1 MiB rotation, three-segment bound and restart append")
    }

    static func clientSchemaAndExport() throws {
        let runtime = try directory("client")
        let logs = runtime.appendingPathComponent("client-diagnostics", isDirectory: true)
        try fm.createDirectory(at: logs, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        var oldest = Data(), middle = Data(), current = Data()
        for index in 0..<4100 {
            var fields = event(index)
            if index == 4099 {
                fields["message_body"] = secret; fields["token"] = secret; fields["error_description"] = secret
            }
            let bytes = try line(fields)
            if index < 1500 { oldest.append(bytes) }
            else if index < 3000 { middle.append(bytes) }
            else { current.append(bytes) }
        }
        let invalid: [(String, Any)] = [
            ("schema_version", 2), ("time_ms", -1), ("correlation_id", secret), ("request_id", secret),
            ("route", "/api/" + secret), ("method", secret), ("stage", secret), ("status", 999),
            ("error_domain", secret), ("error_code", 100000), ("duration_ms", -1)
        ]
        for (key, value) in invalid {
            var fields = event(); fields[key] = value
            let decoded = try JSONDecoder().decode(ClientDiagnosticEvent.self, from: JSONSerialization.data(withJSONObject: fields))
            try expect(!decoded.validated(), "client schema accepted invalid \(key)")
            current.append(try line(fields))
        }
        current.append(Data("2000-01-01T00:00:00Z {broken\n".utf8))
        try write(oldest, to: logs.appendingPathComponent(names[2]))
        try write(middle, to: logs.appendingPathComponent(names[1]))
        try write(current, to: logs.appendingPathComponent(names[0]))
        let diagnostics = ClientDiagnostics(); diagnostics.start(root: runtime)
        let value = try settled(diagnostics) { $0.events.contains { $0.route == "application" && $0.stage == "started" } }
        try expect(value.events.count == 4096 && value.events.contains { $0.timeMS == 4099 }, "newest event export bound changed")
        try expect(!value.events.contains { $0.timeMS == 0 }, "expired oldest event retained")
        try expect(value.invalidRecords == invalid.count + 1, "invalid records were not counted")
        try expect(value.warnings.contains("client_invalid_records_skipped") && value.warnings.contains("client_export_truncated"),
                   "export omitted limitation warnings")
        let bundle = try SupportBundleBuilder.make(client: value, backend: nil, appVersion: secret, build: secret)
        let package = try json(bundle)
        try expect(package["app_version"] as? String == "unknown" && package["build"] as? String == "unknown",
                   "arbitrary version/build text escaped")
        try expect(!String(decoding: bundle, as: UTF8.self).contains(secret), "client support bundle leaked private fields")
        try expect(bundle.count <= 8 * 1024 * 1024, "export exceeded 8 MiB")
        let reopened = ClientDiagnostics(); reopened.start(root: runtime)
        try expect(reopened.snapshot().events.contains { $0.timeMS == 4099 }, "replacement GUI lost history")
        print("PASS client schema: invalid values, extra-field stripping, malformed count, 4096 newest records and restart history")
    }

    static func record(_ sequence: Int, _ event: [String: Any]) -> [String: Any] {
        ["schema_version": 1, "runtime_id": String(repeating: "b", count: 32),
         "sequence": sequence, "time_ms": 1, "event": event]
    }
    static func backendEnvelope() -> [String: Any] {
        ["schema_version": 1, "generated_at_ms": 1, "version": "3.53", "build": "353",
         "source_digest": String(repeating: "a", count: 64), "runtime_id": String(repeating: "b", count: 32),
         "limits": ["queue_capacity": 256, "max_record_bytes": 4096, "segment_bytes": 2_097_152,
                    "segments": 4, "export_bytes": 2_097_152, "max_events": 4096],
         "counters": ["accepted": 3, "written": 3, "dropped": 0, "write_failures": 0, "invalid_records": 0, "read_failures": 0],
         "events": [
            record(1, ["kind": "lifecycle", "phase": "startup"]),
            record(2, ["kind": "api", "route": "/api/sms/send", "method": "POST", "status": 503,
                       "code": "http_server_error", "duration_ms": 2, "correlation_id": String(repeating: "c", count: 32)]),
            record(3, ["kind": "safe_event", "detail": ["sequence": 1, "time_ms": 1, "generation": 2, "duration_ms": 3,
                "correlation_id": String(repeating: "d", count: 32), "operation": "sms_send", "stage": "complete",
                "code": "success", "result": "succeeded"]])],
         "warnings": ["best_effort_not_lossless", "pending_queue_not_flushed", "snapshot_not_atomic", "current_runtime_counters"]]
    }
    static func bundle(_ object: [String: Any]?, client: ClientDiagnosticSnapshot? = nil) throws -> Data {
        try SupportBundleBuilder.make(client: client ?? ClientDiagnostics().snapshot(),
            backend: object.map { try JSONSerialization.data(withJSONObject: $0) }, appVersion: "3.53", build: "353")
    }
    static func bundles() throws {
        let client = ClientDiagnostics().snapshot()
        let valid = try JSONSerialization.data(withJSONObject: backendEnvelope())
        let good = try json(bundle(backendEnvelope()))["backend"] as! [String: Any]
        try expect((good["events"] as? [[String: Any]])?.count == 3, "valid backend event kinds rejected")
        let failures: [Data?] = [nil, Data("{malformed".utf8), Data("[]".utf8), Data(#"{"schema_version":2}"#.utf8),
            Data(repeating: 0x20, count: 2 * 1024 * 1024 + 1)]
        for failure in failures {
            let result = try json(SupportBundleBuilder.make(client: client, backend: failure, appVersion: "3.53", build: "353"))
            try expect(result["backend"] is NSNull && result["client"] is [String: Any], "client fallback failed")
            try expect((result["warnings"] as? [String])?.contains("backend_snapshot_unavailable") == true, "fallback warning missing")
        }
        for badValue in [true, -1, 1.25, secret] as [Any] {
            var broken = backendEnvelope()
            var counters = broken["counters"] as! [String: Any]; counters["written"] = badValue; broken["counters"] = counters
            try expect(try json(bundle(broken))["backend"] is NSNull, "non-integer backend counter accepted")
        }
        var hostile = backendEnvelope()
        hostile["password"] = secret; hostile["warnings"] = ["best_effort_not_lossless", secret]
        for key in ["limits", "counters"] {
            var fields = hostile[key] as! [String: Any]; fields["private_extra"] = secret; hostile[key] = fields
        }
        var records = hostile["events"] as! [[String: Any]]
        for index in records.indices {
            records[index]["message"] = secret
            var detail = records[index]["event"] as! [String: Any]; detail["password"] = secret
            if index == 1 { detail["code"] = secret }
            if index == 2 {
                var safe = detail["detail"] as! [String: Any]; safe["body"] = secret; detail["detail"] = safe
            }
            records[index]["event"] = detail
        }
        records.append(record(4, ["kind": "lifecycle", "phase": secret]))
        records.append(record(5, ["kind": secret]))
        hostile["events"] = records
        let cleaned = try bundle(hostile)
        try expect(!String(decoding: cleaned, as: UTF8.self).contains(secret), "nested backend metadata leaked private input")
        let backend = try json(cleaned)["backend"] as! [String: Any]
        try expect((backend["events"] as? [[String: Any]])?.count == 3, "invalid records not isolated from valid ones")
        try expect((backend["warnings"] as? [String])?.contains("backend_invalid_records_skipped") == true, "invalid record warning missing")
        var tooMany = backendEnvelope()
        tooMany["events"] = Array(repeating: record(1, ["kind": "lifecycle", "phase": "startup"]), count: 4097)
        try expect(try json(bundle(tooMany))["backend"] is NSNull, "backend event count cap ignored")
        let oversized = ClientDiagnosticSnapshot(dropped: 0, writeFailures: 0, invalidRecords: 0,
            events: [], warnings: [String(repeating: "x", count: 9 * 1024 * 1024), secret])
        let bounded = try SupportBundleBuilder.make(client: oversized, backend: valid, appVersion: "3.53", build: "353")
        try expect(bounded.count <= 8 * 1024 * 1024 && !String(decoding: bounded, as: UTF8.self).contains(secret),
                   "constructed client snapshot bypassed export privacy or limit")
        print("PASS bundle: all event kinds, malformed/oversized fallback, integer-only counters, nested privacy and event/byte caps")
    }

    static func unsafeSnapshotPaths() throws {
        for attack in ["directory", "ancestor", "segment", "oversized"] {
            let base = try directory("unsafe-" + attack), outside = base.appendingPathComponent("outside", isDirectory: true)
            try fm.createDirectory(at: outside, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            let external = outside.appendingPathComponent("preserve")
            try write(Data(secret.utf8), to: external)
            let before = try Data(contentsOf: external)
            var runtime = base
            let logs = base.appendingPathComponent("client-diagnostics", isDirectory: true)
            if attack == "directory" { try fm.createSymbolicLink(at: logs, withDestinationURL: outside) }
            else if attack == "ancestor" {
                let alias = base.appendingPathComponent("alias")
                try fm.createSymbolicLink(at: alias, withDestinationURL: outside)
                runtime = alias.appendingPathComponent("nested")
            } else {
                try fm.createDirectory(at: logs, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
                if attack == "segment" { try fm.createSymbolicLink(at: logs.appendingPathComponent(names[1]), withDestinationURL: external) }
                else { try write(Data(repeating: 0x20, count: limit + 1), to: logs.appendingPathComponent(names[0])) }
            }
            let beforeEntries = try fm.contentsOfDirectory(atPath: outside.path)
            let diagnostics = ClientDiagnostics(); diagnostics.start(root: runtime)
            let value = try settled(diagnostics) { $0.writeFailures > 0 }
            try expect(value.events.isEmpty && value.warnings.contains("client_log_read_failed")
                       && value.warnings.contains("client_log_write_failed"), "unsafe snapshot did not fail closed")
            try expect(try Data(contentsOf: external) == before
                && fm.contentsOfDirectory(atPath: outside.path) == beforeEntries, "diagnostics changed an external target")
            try expect(try !String(decoding: bundle(nil, client: value), as: UTF8.self).contains(secret), "unsafe snapshot exported target")
        }
        print("PASS safe failure: directory/ancestor/segment symlinks and oversized files preserve external bytes")
    }
    static func exportLeaseOutlivesCancellation() async throws {
        guard let lease = SupportExportLease.acquire() else {
            throw CheckFailure(description: "initial export lease unavailable")
        }
        let entered = DispatchSemaphore(value: 0)
        let finish = DispatchSemaphore(value: 0)
        let owner = Task {
            defer { lease.release() }
            await Task.detached(priority: .utility) {
                entered.signal()
                // Stand in only for blocked filesystem IO; actual Task and lease
                // lifetime/cancellation semantics remain production Swift code.
                finish.wait()
            }.value
        }
        do {
            try expect(entered.wait(timeout: .now() + 5) == .success, "detached export did not start")
            try expect(SupportExportLease.acquire() == nil, "concurrent export bypassed active lease")
            owner.cancel()
            try expect(owner.isCancelled, "fixture did not cancel export waiter")
            try expect(SupportExportLease.acquire() == nil, "cancelled waiter released still-running filesystem work")
        } catch {
            finish.signal()
            await owner.value
            throw error
        }
        finish.signal()
        await owner.value
        guard let successor = SupportExportLease.acquire() else {
            throw CheckFailure(description: "retired export retained its lease")
        }
        defer { successor.release() }
        lease.release()
        try expect(SupportExportLease.acquire() == nil, "old release cleared successor's active lease")
        successor.release()
        guard let final = SupportExportLease.acquire() else {
            throw CheckFailure(description: "successor did not release after retirement")
        }
        final.release()
        print("PASS export lease: cancelled waiter remains busy until actual detached work retires; stale release cannot unlock successor")
    }
    static func main() async throws {
        try await exportLeaseOutlivesCancellation()
        try rotationAndRestart()
        try clientSchemaAndExport()
        try bundles()
        try unsafeSnapshotPaths()
        print("PASS production support diagnostics; isolated files; no devices, network or real user data")
    }
}
