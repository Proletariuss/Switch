import Foundation
import Darwin

/// Synthetic guardian -> synthetic App -> Rust validator. Never Switch/App.
@main enum SwiftDelegation {
    static func rewrite(_ path: String, _ bytes: Data) throws {
        let descriptor = open(path, O_WRONLY | O_TRUNC | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw AcceptanceContext.Rejection.invalid("fixture-rewrite") }
        let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        try handle.write(contentsOf: bytes); try handle.synchronize(); try handle.close()
    }

    static func main() {
        do {
            let args = CommandLine.arguments
            if (args.count == 4 || args.count == 5) && args[1] == "--synthetic-app" {
                let context = try AcceptanceContext.load(arguments: [args[0], AcceptanceContext.argumentPrefix + args[2]],
                    executablePath: GuardianFixture.selfPath(), expectedBackendPath: args[3])!
                let child = try context.createDaemonCapsule()
                let mode = args.count == 5 ? args[4] : "normal"
                if mode == "alternate-parent" {
                    let alternate = (args[2] as NSString).deletingLastPathComponent + "/alternate.json"
                    let before = try Data(contentsOf: URL(fileURLWithPath: args[2]))
                    var oldIdentity = stat(), newIdentity = stat()
                    guard lstat(args[2], &oldIdentity) == 0 else { exit(1) }
                    try FileManager.default.moveItem(atPath: args[2], toPath: alternate)
                    guard lstat(alternate, &newIdentity) == 0, oldIdentity.st_ino == newIdentity.st_ino,
                          oldIdentity.st_dev == newIdentity.st_dev,
                          try Data(contentsOf: URL(fileURLWithPath: alternate)) == before else { exit(1) }
                    var value = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: child))) as! [String: Any]
                    value["parent_manifest_path"] = alternate
                    try rewrite(child, AcceptanceContext.encode(value))
                } else if mode == "expired-child" {
                    var value = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: child))) as! [String: Any]
                    value["expires_unix_ms"] = Int64(Date().timeIntervalSince1970 * 1000) - 1
                    try rewrite(child, AcceptanceContext.encode(value))
                } else if mode != "normal" { exit(64) }
                let process = Process()
                process.executableURL = URL(fileURLWithPath: args[3])
                process.arguments = ["--kernel-validate", child]
                process.environment = ProcessInfo.processInfo.environment
                let pipe = Pipe(); process.standardError = pipe
                try process.run(); process.waitUntilExit()
                let output = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
                if mode != "normal" {
                    let claims = try FileManager.default.contentsOfDirectory(atPath: context.dataRoot).filter { $0.hasPrefix(".consumed-child-") }
                    guard process.terminationStatus == 78, !output.contains("VALIDATED_CONTEXT_RUNTIME_NO_GO"), claims.isEmpty else {
                        throw AcceptanceContext.Rejection.invalid("Rust-accepted-adversarial-" + mode)
                    }
                    print(mode == "alternate-parent" ? "SWIFT_RENAMED_PARENT_SAME_HASH_INODE_KERNEL_REFUSED_NO_GO" : "SWIFT_EXPIRED_CHILD_KERNEL_REFUSED_NO_GO")
                    return
                }
                guard process.terminationStatus == 78 && output.contains("VALIDATED_CONTEXT_RUNTIME_NO_GO") else {
                    throw AcceptanceContext.Rejection.invalid("Rust-rejected-Swift-delegation")
                }
                // A fresh process cannot consume the same child again.
                let replay = Process(); replay.executableURL = process.executableURL
                replay.arguments = process.arguments; replay.environment = process.environment
                let replayPipe = Pipe(); replay.standardError = replayPipe
                try replay.run(); replay.waitUntilExit()
                let replayOutput = String(data: replayPipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
                guard replay.terminationStatus == 78 && !replayOutput.contains("VALIDATED_CONTEXT_RUNTIME_NO_GO") else {
                    throw AcceptanceContext.Rejection.invalid("Rust-replay")
                }
                print("SWIFT_DAEMON_SCHEMA_KERNEL_PARENT_VALIDATED_REPLAY_REFUSED_NO_GO")
                return
            }
            guard args.count == 3 || args.count == 4 else { exit(64) }
            let (path, original) = try GuardianFixture.make(args[1], guardianPID: getpid())
            var manifest = original
            manifest["backend_path"] = args[2]
            manifest["backend_sha256"] = try AcceptanceContext.hashFile(args[2])
            // Fixtures only: finish the parent before any validator observes it.
            let descriptor = open(path, O_WRONLY | O_TRUNC | O_NOFOLLOW | O_CLOEXEC)
            guard descriptor >= 0 else { exit(1) }
            let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
            try handle.write(contentsOf: AcceptanceContext.encode(manifest)); try handle.synchronize(); try handle.close()
            let app = Process(); app.executableURL = URL(fileURLWithPath: try GuardianFixture.selfPath())
            app.arguments = ["--synthetic-app", path, args[2]] + (args.count == 4 ? [args[3]] : [])
            app.environment = ["HOME": manifest["data_root"] as! String,
                               "CFFIXED_USER_HOME": manifest["data_root"] as! String,
                               "TMPDIR": manifest["tmp_root"] as! String,
                               "PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"]
            try app.run(); app.waitUntilExit(); exit(app.terminationStatus)
        } catch { fputs("Synthetic delegation failed: \(error)\n", stderr); exit(1) }
    }
}
