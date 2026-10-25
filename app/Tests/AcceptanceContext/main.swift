import Foundation
import Darwin

@main
enum AcceptanceContextTests {
    static var passed: [String] = []
    static func require(_ condition: @autoclosure () throws -> Bool, _ name: String) throws {
        guard try condition() else { throw AcceptanceContext.Rejection.invalid("test-" + name) }
        passed.append(name)
    }

    static func main() {
        do {
            guard CommandLine.arguments.count == 2 else { exit(64) }
            let root = CommandLine.arguments[1]
            let fm = FileManager.default
            let executable = try GuardianFixture.selfPath()
            try require(AcceptanceContext.load(arguments: [executable], executablePath: "/not-inspected", expectedBackendPath: "/not-inspected") == nil,
                        "production-no-arguments-no-files")
            setenv("SWITCH_ACCEPTANCE_CAPSULE", "/not-inspected", 1)
            setenv("SWITCH_DATA_DIR", "/not-an-acceptance-authority", 1)
            try require(AcceptanceContext.load(arguments: [executable, "--background"], executablePath: "/not-inspected", expectedBackendPath: "/not-inspected") == nil,
                        "environment-cannot-authorize")
            unsetenv("SWITCH_ACCEPTANCE_CAPSULE")
            unsetenv("SWITCH_DATA_DIR")
            for (name, args) in [
                ("empty-argument", [AcceptanceContext.argumentPrefix]),
                ("separate-argument", ["--switch-acceptance-capsule", "/x"]),
                ("duplicate-argument", [AcceptanceContext.argumentPrefix + "/x", AcceptanceContext.argumentPrefix + "/x"]),
                ("unknown-acceptance-argument", ["--switch-acceptance-unsafe"]),
                ("relative-argument", [AcceptanceContext.argumentPrefix + "relative"]),
                ("dotdot-argument", [AcceptanceContext.argumentPrefix + "/a/../b"]),
                ("double-slash-argument", [AcceptanceContext.argumentPrefix + "/a//b"]),
                ("alternate-basename-argument", [AcceptanceContext.argumentPrefix + "/a/alternate.json"])
            ] { try rejects(name) { _ = try AcceptanceContext.capsuleArgument([executable] + args) } }

            let (alternateSource, _) = try GuardianFixture.make(root + "/alternate-basename")
            let alternatePath = root + "/alternate-basename/alternate.json"
            try fm.moveItem(atPath: alternateSource, toPath: alternatePath)
            try rejects("alternate-basename-identical-manifest") {
                _ = try AcceptanceContext.load(arguments: [executable, AcceptanceContext.argumentPrefix + alternatePath],
                    executablePath: executable, expectedBackendPath: executable, environment: privateEnvironment(alternatePath))
            }

            let (validPath, _) = try GuardianFixture.make(root + "/valid")
            let valid = try AcceptanceContext.load(arguments: [executable, AcceptanceContext.argumentPrefix + validPath], executablePath: executable, expectedBackendPath: executable, environment: privateEnvironment(validPath))
            try require(valid != nil, "valid-private-context")
            let delegated = try valid!.createDaemonCapsule()
            let data = try Data(contentsOf: URL(fileURLWithPath: delegated))
            let child = try JSONSerialization.jsonObject(with: data) as! [String: Any]
            try require(child["role"] as? String == "daemon" && child["app_pid"] as? Int64 == Int64(getpid())
                        && child["parent_manifest_sha256"] as? String == valid!.manifestSHA256
                        && child["parent_manifest_path"] as? String == validPath
                        && AcceptanceContext.hex(child["child_nonce"], count: 64), "daemon-role-parent-fresh-nonce")
            try rejects("replay-nonce") {
                _ = try AcceptanceContext.load(arguments: [executable, AcceptanceContext.argumentPrefix + validPath], executablePath: executable, expectedBackendPath: executable, environment: privateEnvironment(validPath))
            }
            let mutations: [(String, ([String: Any]) -> [String: Any])] = [
                ("unknown-key", { var v = $0; v["override"] = true; return v }),
                ("missing-key", { var v = $0; v.removeValue(forKey: "nonce"); return v }),
                ("wrong-schema", { var v = $0; v["schema_version"] = 2; return v }),
                ("boolean-integer", { var v = $0; v["schema_version"] = true; return v }),
                ("wrong-role", { var v = $0; v["role"] = "daemon"; return v }),
                ("wrong-purpose", { var v = $0; v["purpose"] = "launch-production"; return v }),
                ("parent-pid", { var v = $0; v["guardian_pid"] = Int64(getpid()); return v }),
                ("manifest-owner", { var v = $0; v["uid"] = Int64(getuid()) + 1; return v }),
                ("nonce-format", { var v = $0; v["nonce"] = "invalid"; return v }),
                ("run-id-format", { var v = $0; v["run_id"] = "INVALID"; return v }),
                ("expired", { var v = $0; v["expires_unix_ms"] = 1; return v }),
                ("future-created", { var v = $0; v["created_unix_ms"] = Int64(Date().timeIntervalSince1970 * 1000) + 100000; return v }),
                ("too-long-expiry", { var v = $0; v["expires_unix_ms"] = (v["created_unix_ms"] as! Int64) + 120001; return v }),
                ("wrong-app-digest", { var v = $0; v["app_sha256"] = String(repeating: "0", count: 64); return v }),
                ("wrong-backend-digest", { var v = $0; v["backend_sha256"] = String(repeating: "0", count: 64); return v }),
                ("wrong-guardian-digest", { var v = $0; v["guardian_sha256"] = String(repeating: "0", count: 64); return v }),
                ("guardian-image-path", { var v = $0; v["guardian_path"] = "/wrong-parent-image"; return v }),
                ("bundle-backend-path", { var v = $0; v["backend_path"] = "/other-daemon"; return v }),
                ("app-path-mismatch", { var v = $0; v["app_path"] = "/wrong-image"; return v }),
                ("root-inode", { var v = $0; v["data_root_ino"] = 1; return v }),
                ("root-escape", { var v = $0; v["data_root"] = "/outside"; return v })
            ]
            for (name, mutate) in mutations {
                let (path, manifest) = try GuardianFixture.make(root + "/" + name)
                try rewrite(path, AcceptanceContext.encode(mutate(manifest)))
                try rejects(name) { _ = try AcceptanceContext.load(arguments: [executable, AcceptanceContext.argumentPrefix + path], executablePath: executable, expectedBackendPath: executable, environment: privateEnvironment(path)) }
            }
            for name in ["malformed", "duplicate-key", "symlink", "hardlink", "file-mode", "directory-mode", "read-race", "path-race", "ancestor-symlink", "fifo", "empty", "oversize", "readonly-valid"] {
                let caseRoot = root + "/" + name
                let (path, manifest) = try GuardianFixture.make(caseRoot)
                switch name {
                case "malformed": try rewrite(path, Data("{oops}".utf8))
                case "duplicate-key":
                    let canonical = String(data: try AcceptanceContext.encode(manifest), encoding: .utf8)!
                    try rewrite(path, Data(("{\"role\":\"daemon\"," + canonical.dropFirst()).utf8))
                case "symlink":
                    try fm.moveItem(atPath: path, toPath: caseRoot + "/real.json")
                    try fm.createSymbolicLink(atPath: path, withDestinationPath: "real.json")
                case "hardlink": guard link(path, caseRoot + "/linked.json") == 0 else { throw AcceptanceContext.Rejection.invalid("link") }
                case "file-mode": guard chmod(path, 0o644) == 0 else { throw AcceptanceContext.Rejection.invalid("chmod") }
                case "directory-mode": guard chmod(caseRoot, 0o755) == 0 else { throw AcceptanceContext.Rejection.invalid("chmod-root") }
                case "ancestor-symlink":
                    try fm.moveItem(atPath: caseRoot + "/data", toPath: caseRoot + "/original-data")
                    try fm.createSymbolicLink(atPath: caseRoot + "/data", withDestinationPath: "original-data")
                case "fifo": try fm.removeItem(atPath: path); guard mkfifo(path, 0o600) == 0 else { throw AcceptanceContext.Rejection.invalid("fifo") }
                case "empty": try rewrite(path, Data())
                case "oversize": try rewrite(path, Data(repeating: 65, count: 16385))
                case "readonly-valid": guard chmod(path, 0o400) == 0 else { throw AcceptanceContext.Rejection.invalid("chmod-readonly") }
                default: break
                }
                let action = {
                    _ = try AcceptanceContext.load(arguments: [executable, AcceptanceContext.argumentPrefix + path], executablePath: executable, expectedBackendPath: executable, environment: privateEnvironment(path), afterRead: {
                        if name == "read-race" { try rewrite(path, Data("{}".utf8)) }
                        if name == "path-race" {
                            try fm.moveItem(atPath: path, toPath: caseRoot + "/before.json")
                            let directory = try AcceptanceContext.openDirectory(caseRoot)
                            defer { close(directory.0) }
                            try AcceptanceContext.writeNew(try AcceptanceContext.encode(manifest), name: "app.json", directoryFD: directory.0)
                        }
                    })
                }
                if name == "readonly-valid" { try action(); passed.append(name) } else { try rejects(name, action) }
            }
            for key in ["HOME", "CFFIXED_USER_HOME", "TMPDIR"] {
                for missing in [true, false] {
                    let label = "environment-" + key + (missing ? "-missing" : "-mismatch")
                    let (path, _) = try GuardianFixture.make(root + "/" + label)
                    var environment = privateEnvironment(path)
                    environment[key] = missing ? nil : "/wrong-root"
                    try rejects(label) {
                        _ = try AcceptanceContext.load(arguments: [executable, AcceptanceContext.argumentPrefix + path],
                            executablePath: executable, expectedBackendPath: executable, environment: environment)
                    }
                }
            }
            // Mutate only fixture paths at the exact post-hash boundary. No
            // scheduler timing, sparse-file size or concurrent thread is needed.
            for name in ["binary-ancestor-replacement", "binary-leaf-replacement",
                         "binary-leaf-symlink", "binary-ancestor-symlink",
                         "binary-initial-leaf-symlink", "binary-initial-ancestor-symlink"] {
                let caseRoot = root + "/" + name
                let ancestor = caseRoot + "/ancestor"
                let path = ancestor + "/image"
                try fm.createDirectory(atPath: ancestor, withIntermediateDirectories: true)
                try Data("original image".utf8).write(to: URL(fileURLWithPath: path))
                var mutationCompleted = false
                let mutate = {
                    if name.contains("ancestor") {
                        try fm.moveItem(atPath: ancestor, toPath: caseRoot + "/old-ancestor")
                        if name.contains("symlink") {
                            try fm.createSymbolicLink(atPath: ancestor, withDestinationPath: "old-ancestor")
                        } else {
                            try fm.createDirectory(atPath: ancestor, withIntermediateDirectories: false)
                            try Data("replacement image".utf8).write(to: URL(fileURLWithPath: path))
                        }
                    } else {
                        try fm.moveItem(atPath: path, toPath: ancestor + "/old-image")
                        if name.contains("symlink") {
                            try fm.createSymbolicLink(atPath: path, withDestinationPath: "old-image")
                        } else {
                            try Data("replacement image".utf8).write(to: URL(fileURLWithPath: path))
                        }
                    }
                    mutationCompleted = true
                }
                if name.contains("initial") {
                    try mutate()
                    try rejects(name) { _ = try AcceptanceContext.hashFile(path) }
                } else {
                    try require(try AcceptanceContext.hashFile(path) == AcceptanceContext.hashFile(path), name + "-baseline")
                    try rejects(name) { _ = try AcceptanceContext.hashFile(path, afterHash: mutate) }
                    try require(mutationCompleted, name + "-mutation-completed")
                }
            }
            let systemFD = open("/usr/bin/true", O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
            guard systemFD >= 0 else { throw AcceptanceContext.Rejection.invalid("owner-fixture") }
            defer { close(systemFD) }
            try rejects("kernel-file-owner-mismatch") { _ = try AcceptanceContext.fileIdentity(systemFD, privateFile: false) }
            let result: [String: Any] = ["scope": "pure-fixture-production-validator", "passed": true,
                "count": passed.count, "checks": passed, "actual_app_launched": false, "actual_daemon_launched": false,
                "formal_duration_credit_seconds": 0]
            FileHandle.standardOutput.write(try AcceptanceContext.encode(result))
            FileHandle.standardOutput.write(Data([10]))
        } catch {
            fputs("AcceptanceContext test failure: \(error)\n", stderr)
            exit(1)
        }
    }
    static func rewrite(_ path: String, _ bytes: Data) throws {
        // Test-only corruption writer; never used by the actual validator.
        let fd = open(path, O_WRONLY | O_TRUNC | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw AcceptanceContext.Rejection.invalid("fixture-rewrite") }
        defer { close(fd) }
        let written = bytes.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard written == bytes.count, fsync(fd) == 0 else { throw AcceptanceContext.Rejection.invalid("fixture-write") }
    }
    static func privateEnvironment(_ path: String) -> [String: String] {
        let root = (path as NSString).deletingLastPathComponent
        return ["HOME": root + "/data", "CFFIXED_USER_HOME": root + "/data", "TMPDIR": root + "/tmp"]
    }
    static func rejects(_ name: String, _ action: () throws -> Void) throws {
        do { try action() } catch { passed.append(name); return }
        throw AcceptanceContext.Rejection.invalid("accepted-negative-" + name)
    }
}
