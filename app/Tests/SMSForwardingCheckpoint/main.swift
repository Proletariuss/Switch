import Darwin
import Foundation

// Compile this file and the unchanged production checkpoint implementation
// with -D SWITCH_CHECKPOINT_TESTS. Run only in the existing hardware/network
// deny sandbox, with SWITCH_BUILD_DIR under this checkout. All evidence stays
// in a fresh build/runtime directory; no preferences or real user data is read.
@main
struct CheckpointTests {
    enum TestFailure: Error { case assertion(String) }

    static func require(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
        guard try value() else { throw TestFailure.assertion(message) }
    }

    static func expect(_ expected: SMSForwardingCheckpoint.Failure, _ operation: () throws -> Void) throws {
        do {
            try operation()
        } catch let actual as SMSForwardingCheckpoint.Failure {
            try require(actual == expected, "wrong checkpoint failure: \(actual), expected \(expected)")
            return
        }
        throw TestFailure.assertion("checkpoint operation unexpectedly succeeded")
    }

    static func makeDirectory(_ path: URL) throws {
        try FileManager.default.createDirectory(at: path, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: NSNumber(value: 0o700)])
    }

    static func information(_ path: URL) throws -> stat {
        var value = stat()
        guard path.path.withCString({ Darwin.lstat($0, &value) }) == 0 else {
            throw TestFailure.assertion("fixture lstat failed: \(path.path)")
        }
        return value
    }

    static func rawFile(_ path: URL, _ bytes: Data, mode: mode_t = 0o600) throws {
        try bytes.write(to: path)
        try require(path.path.withCString({ Darwin.chmod($0, mode) }) == 0, "fixture chmod failed")
    }

    static func buildDirectory() throws -> URL {
        guard let value = ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"], value.hasPrefix("/") else {
            throw TestFailure.assertion("requires workspace SWITCH_BUILD_DIR and mandatory OS sandbox")
        }
        let build = URL(fileURLWithPath: value, isDirectory: true).resolvingSymlinksInPath().standardizedFileURL
        var workspace = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { workspace.deleteLastPathComponent() }
        let expected = workspace.appendingPathComponent("build", isDirectory: true).resolvingSymlinksInPath().standardizedFileURL
        try require(build == expected, "test build root must belong to this Switch_MAC checkout; compile absolute source paths")
        return build
    }

    static func waitFor(_ condition: () -> Bool, _ message: String) throws {
        let deadline = Date().addingTimeInterval(8)
        while !condition() {
            try require(Date() < deadline, message)
            Thread.sleep(forTimeInterval: 0.005)
        }
    }

    static func child(_ arguments: [String], build: URL) throws {
        try require(arguments.count == 7, "bad checkpoint child arguments")
        let directory = URL(fileURLWithPath: arguments[2], isDirectory: true).standardizedFileURL
        let ready = URL(fileURLWithPath: arguments[4]).standardizedFileURL
        let gate = URL(fileURLWithPath: arguments[5]).standardizedFileURL
        let result = URL(fileURLWithPath: arguments[6]).standardizedFileURL
        for path in [directory, ready, gate, result] {
            try require(path.path.hasPrefix(build.path + "/"), "child path escaped build root")
        }
        let checkpoint = SMSForwardingCheckpoint(directory: directory)
        try Data("ready".utf8).write(to: ready)
        try waitFor({ FileManager.default.fileExists(atPath: gate.path) }, "child start gate deadline")
        let deadline = Date().addingTimeInterval(8)
        while true {
            do {
                let bytes = try checkpoint.confirm(Data(arguments[3].utf8))
                // This is only the independent process's observation receipt.
                // The checkpoint authority itself was written by production IO.
                try bytes.write(to: result)
                return
            } catch SMSForwardingCheckpoint.Failure.busy {
                try require(Date() < deadline, "child checkpoint contention deadline")
                Thread.sleep(forTimeInterval: 0.005)
            }
        }
    }

    static func spawn(_ arguments: [String], log: URL) throws -> Process {
        let executable = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        FileManager.default.createFile(atPath: log.path, contents: nil)
        let output = try FileHandle(forWritingTo: log)
        defer { try? output.close() }
        process.standardOutput = output
        process.standardError = output
        try process.run()
        return process
    }

    static func stopOwnedChildren(_ processes: [Process]) {
        for process in processes where process.isRunning { process.terminate() }
        let deadline = Date().addingTimeInterval(2)
        while processes.contains(where: { $0.isRunning }) && Date() < deadline {
            Thread.sleep(forTimeInterval: 0.005)
        }
        for process in processes {
            if process.isRunning { _ = Darwin.kill(process.processIdentifier, SIGKILL) }
            process.waitUntilExit()
        }
    }

    static func reap(_ processes: [Process]) throws {
        do {
            try waitFor({ processes.allSatisfy { !$0.isRunning } }, "owned checkpoint child process deadline")
        } catch {
            // Only processes created by this exact test are eligible. Forced
            // termination remains a failed test, never successful acceptance.
            stopOwnedChildren(processes)
            throw error
        }
        for process in processes {
            process.waitUntilExit()
            try require(process.terminationReason == .exit && process.terminationStatus == 0,
                        "owned child \(process.processIdentifier) did not exit zero")
        }
    }

    static func main() throws {
        #if !SWITCH_CHECKPOINT_TESTS
        throw TestFailure.assertion("checkpoint tests require -D SWITCH_CHECKPOINT_TESTS")
        #else
        let build = try buildDirectory()
        if CommandLine.arguments.dropFirst().first == "--confirm" {
            try child(CommandLine.arguments, build: build)
            return
        }
        if CommandLine.arguments.dropFirst().first == "--crash-visible" {
            try require(CommandLine.arguments.count == 3, "bad crash child arguments")
            let directory = URL(fileURLWithPath: CommandLine.arguments[2]).standardizedFileURL
            try require(directory.path.hasPrefix(build.path + "/"), "crash child escaped build root")
            try expect(.unavailable) {
                _ = try SMSForwardingCheckpoint(directory: directory, failing: .directorySync)
                    .confirm(Data("visible-before-durable-ack".utf8))
            }
            Darwin._exit(86) // No Swift teardown; expected failure, never an ACK.
        }
        let parent = build.appendingPathComponent("runtime", isDirectory: true)
        let root = parent.appendingPathComponent("checkpoint-580-\(UUID().uuidString.lowercased())", isDirectory: true)
        try makeDirectory(root)
        var passed = [String]()
        func run(_ name: String, _ test: (URL) throws -> Void) throws {
            let directory = root.appendingPathComponent(name, isDirectory: true)
            try makeDirectory(directory)
            try test(directory)
            passed.append(name)
            print("PASS \(name)")
        }
        let first = Data("first frozen checkpoint".utf8)
        let second = Data("changed source must not replace it".utf8)

        try run("real_persistence_reopen_and_first_winner") { directory in
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            let absent = try checkpoint.loadConfirmed()
            try require(absent == nil, "fresh checkpoint must be absent")
            let created = try checkpoint.confirm(first)
            try require(created == first, "first write changed raw bytes")
            let file = directory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            let before = try information(file)
            try require(before.st_mode & mode_t(0o7777) == mode_t(0o600) && before.st_nlink == 1,
                        "new checkpoint must be private with exactly one link")
            let reopened = SMSForwardingCheckpoint(directory: directory)
            let recovered = try reopened.loadConfirmed()
            let winner = try reopened.confirm(second)
            let after = try information(file)
            try require(recovered == first && winner == first, "reopen/retry replaced the frozen winner")
            try require(before.st_dev == after.st_dev && before.st_ino == after.st_ino,
                        "confirm replaced existing checkpoint inode")
        }

        try run("symlink_checkpoint_preserves_target") { directory in
            let target = directory.appendingPathComponent("unrelated")
            try rawFile(target, first)
            let file = directory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            try FileManager.default.createSymbolicLink(at: file, withDestinationURL: target)
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            try expect(.unavailable) { _ = try checkpoint.loadConfirmed() }
            try expect(.unavailable) { _ = try checkpoint.confirm(second) }
            let bytes = try Data(contentsOf: target)
            let info = try information(file)
            try require(bytes == first && info.st_mode & mode_t(S_IFMT) == mode_t(S_IFLNK), "symlink target or entry changed")
        }

        try run("hardlink_checkpoint_preserves_both_entries") { directory in
            let target = directory.appendingPathComponent("unrelated")
            try rawFile(target, first)
            let file = directory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            try FileManager.default.linkItem(at: target, to: file)
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            try expect(.unavailable) { _ = try checkpoint.confirm(second) }
            let bytes = try Data(contentsOf: target)
            let info = try information(file)
            try require(bytes == first && info.st_nlink == 2, "hardlink authority was adopted or altered")
        }

        try run("symlink_lock_is_rejected") { directory in
            let target = directory.appendingPathComponent("unrelated")
            try rawFile(target, first)
            let lock = directory.appendingPathComponent(SMSForwardingCheckpoint.lockName)
            try FileManager.default.createSymbolicLink(at: lock, withDestinationURL: target)
            try expect(.unavailable) { _ = try SMSForwardingCheckpoint(directory: directory).confirm(second) }
            let bytes = try Data(contentsOf: target)
            try require(bytes == first, "lock symlink changed its target")
        }

        try run("hardlink_lock_is_rejected_without_altering_target") { directory in
            let target = directory.appendingPathComponent("unrelated")
            try rawFile(target, first)
            let lock = directory.appendingPathComponent(SMSForwardingCheckpoint.lockName)
            try FileManager.default.linkItem(at: target, to: lock)
            try expect(.unavailable) { _ = try SMSForwardingCheckpoint(directory: directory).confirm(second) }
            let bytes = try Data(contentsOf: target)
            let info = try information(lock)
            try require(bytes == first && info.st_nlink == 2, "hardlinked lock was adopted or changed")
        }

        try run("missing_directory_is_not_implicitly_created") { directory in
            let missing = directory.appendingPathComponent("missing", isDirectory: true)
            let checkpoint = SMSForwardingCheckpoint(directory: missing)
            try expect(.unavailable) { _ = try checkpoint.loadConfirmed() }
            try expect(.unavailable) { _ = try checkpoint.confirm(first) }
            try require(!FileManager.default.fileExists(atPath: missing.path), "checkpoint created an unowned root")
        }

        try run("symlink_root_does_not_adopt_target_directory") { directory in
            let target = directory.appendingPathComponent("target", isDirectory: true)
            try makeDirectory(target)
            let alias = directory.appendingPathComponent("alias", isDirectory: true)
            try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: target)
            let checkpoint = SMSForwardingCheckpoint(directory: alias)
            try expect(.unavailable) { _ = try checkpoint.confirm(first) }
            let contents = try FileManager.default.contentsOfDirectory(atPath: target.path)
            try require(contents.isEmpty, "symlink directory target was mutated")
        }

        try run("oversized_candidate_creates_no_checkpoint") { directory in
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            let tooLarge = Data(repeating: 65, count: SMSForwardingCheckpoint.maximumBytes + 1)
            try expect(.tooLarge) { _ = try checkpoint.confirm(tooLarge) }
            try require(!FileManager.default.fileExists(atPath: directory.appendingPathComponent(SMSForwardingCheckpoint.fileName).path), "oversized candidate committed")
        }

        try run("oversized_existing_is_not_replaced") { directory in
            let file = directory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            try rawFile(file, Data(repeating: 65, count: SMSForwardingCheckpoint.maximumBytes + 1))
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            try expect(.tooLarge) { _ = try checkpoint.loadConfirmed() }
            try expect(.tooLarge) { _ = try checkpoint.confirm(second) }
            let info = try information(file)
            try require(info.st_size == off_t(SMSForwardingCheckpoint.maximumBytes + 1), "oversized existing checkpoint overwritten")
        }

        try run("exact_size_limit_is_real_durable_io") { directory in
            let bytes = Data(repeating: 65, count: SMSForwardingCheckpoint.maximumBytes)
            let committed = try SMSForwardingCheckpoint(directory: directory).confirm(bytes)
            let reopened = try SMSForwardingCheckpoint(directory: directory).loadConfirmed()
            try require(committed == bytes && reopened == bytes, "8 MiB boundary did not roundtrip actual bytes")
        }

        try run("existing_permissions_are_rejected_without_repair") { directory in
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            _ = try checkpoint.confirm(first)
            let file = directory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            try require(file.path.withCString({ Darwin.chmod($0, 0o644) }) == 0, "fixture chmod")
            try expect(.unavailable) { _ = try checkpoint.loadConfirmed() }
            let info = try information(file)
            let bytes = try Data(contentsOf: file)
            try require(info.st_mode & mode_t(0o7777) == 0o644 && bytes == first, "invalid existing mode was silently repaired")
        }
        try run("existing_lock_permissions_are_rejected_without_repair") { directory in
            let lock = directory.appendingPathComponent(SMSForwardingCheckpoint.lockName)
            try rawFile(lock, Data(), mode: 0o644)
            try expect(.unavailable) { _ = try SMSForwardingCheckpoint(directory: directory).confirm(first) }
            try require(try information(lock).st_mode & mode_t(0o7777) == 0o644, "existing lock mode repaired")
            try require(!FileManager.default.fileExists(atPath: directory.appendingPathComponent(SMSForwardingCheckpoint.fileName).path), "unsafe lock permitted commit")
        }

        try run("root_path_replacement_does_not_rebase_existing_handle") { directory in
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            _ = try checkpoint.confirm(first)
            let moved = directory.appendingPathExtension("moved")
            try FileManager.default.moveItem(at: directory, to: moved)
            try makeDirectory(directory)
            try expect(.unavailable) { _ = try checkpoint.loadConfirmed() }
            try expect(.unavailable) { _ = try checkpoint.confirm(second) }
            let movedBytes = try Data(contentsOf: moved.appendingPathComponent(SMSForwardingCheckpoint.fileName))
            try require(movedBytes == first, "original pinned checkpoint changed")
            try require(try FileManager.default.contentsOfDirectory(atPath: directory.path).isEmpty, "replacement root acquired checkpoint/lock")
        }

        try run("insecure_directory_is_not_adopted") { directory in
            try require(directory.path.withCString({ Darwin.chmod($0, 0o777) }) == 0, "fixture chmod")
            try expect(.unavailable) { _ = try SMSForwardingCheckpoint(directory: directory).loadConfirmed() }
            try require(try FileManager.default.contentsOfDirectory(atPath: directory.path).isEmpty, "unsafe root mutated")
        }

        try run("busy_lock_fails_without_blocking_or_creating_checkpoint") { directory in
            let checkpoint = SMSForwardingCheckpoint(directory: directory)
            _ = try checkpoint.loadConfirmed()
            let lock = directory.appendingPathComponent(SMSForwardingCheckpoint.lockName)
            let descriptor = lock.path.withCString { Darwin.open($0, O_RDWR | O_NOFOLLOW | O_CLOEXEC) }
            try require(descriptor >= 0, "fixture lock open")
            defer { _ = Darwin.close(descriptor) }
            try require(SMSForwardingCheckpoint.lockFileForTesting(descriptor, LOCK_EX | LOCK_NB) == 0, "fixture lock acquire")
            defer { _ = SMSForwardingCheckpoint.lockFileForTesting(descriptor, LOCK_UN) }
            try expect(.busy) { _ = try checkpoint.confirm(first) }
            try require(!FileManager.default.fileExists(atPath: directory.appendingPathComponent(SMSForwardingCheckpoint.fileName).path), "busy lock allowed commit")
        }

        try run("file_sync_failure_never_commits_or_leaks_current_temp") { directory in
            let failing = SMSForwardingCheckpoint(directory: directory, failing: .fileSync)
            try expect(.unavailable) { _ = try failing.confirm(first) }
            try require(!FileManager.default.fileExists(atPath: directory.appendingPathComponent(SMSForwardingCheckpoint.fileName).path), "failed file sync committed")
            let entries = try FileManager.default.contentsOfDirectory(atPath: directory.path)
            try require(!entries.contains { $0.hasSuffix(".tmp") }, "current owned temporary leaked")
            let committed = try SMSForwardingCheckpoint(directory: directory).confirm(first)
            try require(committed == first, "real retry failed after file-sync failure")
        }

        try run("directory_sync_failure_keeps_committed_bytes_for_real_retry") { directory in
            let failing = SMSForwardingCheckpoint(directory: directory, failing: .directorySync)
            try expect(.unavailable) { _ = try failing.confirm(first) }
            let file = directory.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            let visible = try Data(contentsOf: file)
            let before = try information(file)
            try require(visible == first && before.st_nlink == 1, "uncertain commit was deleted or retained a second hardlink")
            let real = SMSForwardingCheckpoint(directory: directory)
            let recovered = try real.loadConfirmed()
            let winner = try real.confirm(second)
            let after = try information(file)
            try require(recovered == first && winner == first && before.st_ino == after.st_ino,
                        "real durable confirmation rotated an uncertain commit")
            try expect(.unavailable) { _ = try SMSForwardingCheckpoint(directory: directory, failing: .fileSync).loadConfirmed() }
            try expect(.unavailable) { _ = try failing.loadConfirmed() }
            let final = try real.loadConfirmed()
            try require(final == first, "existing-file sync failure changed committed bytes")
        }

        try run("independent_processes_keep_exact_first_winner") { directory in
            let checkpointDirectory = directory.appendingPathComponent("authority", isDirectory: true)
            try makeDirectory(checkpointDirectory)
            let gate = directory.appendingPathComponent("go")
            let readyA = directory.appendingPathComponent("ready-a")
            let readyB = directory.appendingPathComponent("ready-b")
            let outputA = directory.appendingPathComponent("a.observed")
            let outputB = directory.appendingPathComponent("b.observed")
            let a = try spawn(["--confirm", checkpointDirectory.path, "A", readyA.path, gate.path, outputA.path], log: directory.appendingPathComponent("a.log"))
            var children = [a]
            defer { stopOwnedChildren(children) }
            let b = try spawn(["--confirm", checkpointDirectory.path, "B", readyB.path, gate.path, outputB.path], log: directory.appendingPathComponent("b.log"))
            children.append(b)
            try waitFor({ FileManager.default.fileExists(atPath: readyA.path) && FileManager.default.fileExists(atPath: readyB.path) }, "independent writers not ready")
            try Data("go".utf8).write(to: gate)
            try reap([a, b])
            let observedA = try Data(contentsOf: outputA)
            let observedB = try Data(contentsOf: outputB)
            let persisted = try SMSForwardingCheckpoint(directory: checkpointDirectory).loadConfirmed()
            try require(observedA == observedB && persisted == observedA && [Data("A".utf8), Data("B".utf8)].contains(observedA), "independent writers diverged or replaced the first winner")
            print("REAPED checkpoint children \(a.processIdentifier), \(b.processIdentifier)")
        }
        try run("abrupt_exit_after_uncertain_commit_keeps_first_inode_and_bytes") { directory in
            let authority = directory.appendingPathComponent("authority", isDirectory: true)
            try makeDirectory(authority)
            let writer = try spawn(["--crash-visible", authority.path], log: directory.appendingPathComponent("crash.log"))
            defer { stopOwnedChildren([writer]) }
            try waitFor({ !writer.isRunning }, "crash writer deadline")
            writer.waitUntilExit()
            try require(writer.terminationReason == .exit && writer.terminationStatus == 86,
                        "crash writer did not reach the intentional pre-ACK exit")
            let file = authority.appendingPathComponent(SMSForwardingCheckpoint.fileName)
            let before = try information(file)
            let normal = SMSForwardingCheckpoint(directory: authority)
            try require(try normal.loadConfirmed() == Data("visible-before-durable-ack".utf8), "restart lost uncertain visible bytes")
            try require(try normal.confirm(second) == Data("visible-before-durable-ack".utf8), "restart replaced first committed bytes")
            try require(try information(file).st_ino == before.st_ino, "restart rotated original inode")
        }

        let report: [String: Any] = ["passed": true, "tests": passed, "count": passed.count,
                                    "root": root.path, "scope": "real production checkpoint file IO and isolated independent-process receipts; no preferences, providers or hardware"]
        try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys, .prettyPrinted]).write(to: root.appendingPathComponent("result.json"))
        print("Checkpoint tests passed: \(passed.count); evidence: \(root.path)")
        #endif
    }
}
