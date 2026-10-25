import Darwin
import Foundation

// The production shared logger must never be used by this harness.
enum SwitchRuntimePaths {
    static func dataDirectory() -> URL { fatalError("test must inject a workspace directory") }
}

private final class Recorder: @unchecked Sendable {
    private let lock = NSLock()
    private var events: [Data] = []
    func append(_ event: Data) { lock.lock(); events.append(event); lock.unlock() }
    var values: [Data] { lock.lock(); defer { lock.unlock() }; return events }
}

private final class ChildReadResult: @unchecked Sendable {
    private let lock = NSLock()
    private var bytes = Data()
    func set(_ value: Data) { lock.lock(); bytes = value; lock.unlock() }
    var value: Data { lock.lock(); defer { lock.unlock() }; return bytes }
}

@main private enum BoundedBLEDiagnosticHarness {
    static let fm = FileManager.default
    static let root: URL = {
        let build = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"]!)
            .resolvingSymlinksInPath()
        let path = URL(fileURLWithPath: ProcessInfo.processInfo.environment["BLE_DIAGNOSTIC_TEST_ROOT"]!)
            .standardizedFileURL.resolvingSymlinksInPath()
        precondition(path.path.hasPrefix(build.path + "/"))
        try! fm.createDirectory(at: path, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        return path
    }()

    static func directory(_ name: String) -> URL {
        let value = root.appendingPathComponent(name)
        try! fm.createDirectory(at: value, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        return value
    }
    static func file(_ path: URL, bytes: Data = Data("original".utf8)) {
        try! bytes.write(to: path)
        precondition(chmod(path.path, 0o600) == 0)
    }
    static func emit(_ logger: BoundedBLEDiagnosticLog, _ message: String = "fixture-only event") {
        precondition(logger.enqueue(message))
        logger.waitUntilIdleForTesting()
    }

    static func queueAdmissionAndFailures() {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let recorder = Recorder()
        let logger = BoundedBLEDiagnosticLog(directory: { fatalError("sink must replace disk IO") }) { data in
            if recorder.values.isEmpty { entered.signal(); release.wait() }
            recorder.append(data)
        }
        precondition(logger.enqueue("blocked writer"))
        precondition(entered.wait(timeout: .now() + 2) == .success)
        let start = DispatchTime.now().uptimeNanoseconds
        for _ in 1..<BoundedBLEDiagnosticLog.capacity { precondition(logger.enqueue("queued")) }
        for _ in 0..<1_000 { precondition(!logger.enqueue("full queue must drop")) }
        precondition(DispatchTime.now().uptimeNanoseconds - start < 1_000_000_000,
                     "enqueue waited for a blocked diagnostic sink")
        release.signal()
        logger.waitUntilIdleForTesting()
        precondition(recorder.values.count == 32)
        emit(logger, "capacity returned after drain")
        precondition(recorder.values.count == 33)

        enum IOFailure: Error { case simulated }
        let failures = Recorder()
        let broken = BoundedBLEDiagnosticLog(directory: { fatalError() }) { data in
            failures.append(data)
            throw IOFailure.simulated
        }
        for _ in 0..<96 { emit(broken) }
        precondition(failures.values.count == 96, "IO failure leaked an admission slot")
        print("PASS production queue: 32 running+queued slots, immediate full drops, IO-failure slot release")
    }

    static func utf8Bounds() {
        let recorder = Recorder()
        let logger = BoundedBLEDiagnosticLog(directory: { fatalError() }, sink: recorder.append)
        precondition(!logger.enqueue(String(repeating: "x", count: 2_027)))
        precondition(!logger.enqueue(String(repeating: "界", count: 676)))
        emit(logger, String(repeating: "x", count: 2_026))
        emit(logger, String(repeating: "界", count: 675))
        let events = recorder.values
        precondition(events.map(\.count) == [2_048, 2_047])
        precondition(events.allSatisfy { String(data: $0, encoding: .utf8) != nil && $0.last == 0x0a })
        print("PASS production UTF-8: event including timestamp/LF <= 2 KiB; oversized messages drop intact")
    }

    static func diskRotationAndContention() {
        let exactRoot = directory("exact-rollover")
        let exact = BoundedBLEDiagnosticLog(directory: { exactRoot })
        for _ in 0..<32 { emit(exact, String(repeating: "x", count: 2_026)) }
        let exactCurrent = exactRoot.appendingPathComponent("ble_diag.log")
        let exactArchive = exactRoot.appendingPathComponent("ble_diag.log.1")
        precondition((try! Data(contentsOf: exactCurrent)).count == 65_536)
        precondition(!fm.fileExists(atPath: exactArchive.path))
        emit(exact, String(repeating: "x", count: 2_026))
        precondition((try! Data(contentsOf: exactCurrent)).count == 2_048
                     && (try! Data(contentsOf: exactArchive)).count == 65_536)

        let target = root.appendingPathComponent("writer-created/subdirectory")
        let logger = BoundedBLEDiagnosticLog(directory: {
            precondition(!Thread.isMainThread, "runtime directory resolved on BLE caller")
            return target
        })
        for index in 0..<240 { emit(logger, "event-\(index) " + String(repeating: "x", count: 1_900)) }
        let entries = try! fm.contentsOfDirectory(atPath: target.path)
        precondition(Set(entries) == Set(BoundedBLEDiagnosticLog.fileNames + [".ble_diag.lock"]))
        var total = 0
        for name in BoundedBLEDiagnosticLog.fileNames {
            let path = target.appendingPathComponent(name)
            let bytes = try! Data(contentsOf: path)
            precondition(!bytes.isEmpty && bytes.count <= 65_536 && bytes.last == 0x0a)
            var info = stat(); precondition(lstat(path.path, &info) == 0)
            precondition(BoundedBLEDiagnosticLog.isPrivateRegularFile(info))
            total += bytes.count
        }
        precondition(total <= 3 * 65_536)
        let current = target.appendingPathComponent("ble_diag.log")
        precondition((try! String(contentsOf: current, encoding: .utf8)).contains("event-239"))
        let lockPath = target.appendingPathComponent(".ble_diag.lock")
        let lock = open(lockPath.path, O_RDWR | O_NOFOLLOW | O_CLOEXEC)
        precondition(lock >= 0 && flock(lock, LOCK_EX | LOCK_NB) == 0)
        let before = try! Data(contentsOf: current)
        emit(logger, "contended writer drops this event")
        precondition((try! Data(contentsOf: current)) == before)
        precondition(flock(lock, LOCK_UN) == 0); close(lock)
        emit(logger, "uncontended writer resumes")
        precondition((try! Data(contentsOf: current)) != before)
        print("PASS production disk path: writer-only directory creation; 64 KiB x 3 rotation; nonblocking OS lock")
    }

    static func childWriter(_ mode: String) {
        let environment = ProcessInfo.processInfo.environment
        let build = URL(fileURLWithPath: environment["SWITCH_BUILD_DIR"]!).resolvingSymlinksInPath()
        let destination = URL(fileURLWithPath: environment["BLE_DIAGNOSTIC_CHILD_ROOT"]!)
            .standardizedFileURL.resolvingSymlinksInPath()
        precondition(destination.path.hasPrefix(build.path + "/"))
        let logger = BoundedBLEDiagnosticLog(directory: { destination })
        let tag = environment["BLE_DIAGNOSTIC_CHILD_TAG"]!
        if mode == "rotate" {
            for index in 0..<240 { emit(logger, "child-\(tag)-\(index) " + String(repeating: "r", count: 1_900)) }
            FileHandle.standardOutput.write(Data("ready\n".utf8))
            precondition(FileHandle.standardInput.readData(ofLength: 1).count == 1)
            emit(logger, "END-child-\(tag)")
        } else {
            precondition(mode == "contended" || mode == "append")
            for _ in 0..<(mode == "contended" ? 40 : 1) { emit(logger, "child-\(tag)-probe") }
        }
    }

    static func realProcesses() {
        let base = directory("real-processes"), logs = base.appendingPathComponent("logs")
        try! fm.createDirectory(at: logs, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let external = base.appendingPathComponent("external-target")
        file(external, bytes: Data("external target remains immutable".utf8))
        let originalExternal = try! Data(contentsOf: external)
        let unrelated = logs.appendingPathComponent("unrelated.log")
        file(unrelated, bytes: Data("unrelated log remains immutable".utf8))
        let originalUnrelated = try! Data(contentsOf: unrelated)
        let logger = BoundedBLEDiagnosticLog(directory: { logs })
        emit(logger, "parent fixture seed")
        typealias Child = (process: Process, output: Pipe, input: Pipe, exited: DispatchSemaphore)
        var reaped: [Int32] = []
        func launch(_ mode: String, tag: String) -> Child {
            let process = Process(), output = Pipe(), input = Pipe(), exited = DispatchSemaphore(value: 0)
            process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
            var environment = ProcessInfo.processInfo.environment
            environment["BLE_DIAGNOSTIC_CHILD_MODE"] = mode
            environment["BLE_DIAGNOSTIC_CHILD_ROOT"] = logs.path
            environment["BLE_DIAGNOSTIC_CHILD_TAG"] = tag
            process.environment = environment
            process.standardOutput = output; process.standardInput = input
            process.standardError = FileHandle.standardError
            process.terminationHandler = { _ in exited.signal() }
            try! process.run()
            return (process, output, input, exited)
        }
        func finish(_ child: Child, seconds: Double = 10) {
            if child.exited.wait(timeout: .now() + seconds) != .success {
                child.process.terminate()
                child.process.waitUntilExit()
                preconditionFailure("owned diagnostic child did not finish within its bounded budget")
            }
            child.process.waitUntilExit()
            reaped.append(child.process.processIdentifier)
            precondition(child.process.terminationReason == .exit && child.process.terminationStatus == 0)
        }
        func ready(_ child: Child) {
            let received = ChildReadResult(), signal = DispatchSemaphore(value: 0)
            DispatchQueue.global(qos: .utility).async {
                received.set(child.output.fileHandleForReading.readData(ofLength: 6))
                signal.signal()
            }
            if signal.wait(timeout: .now() + 10) != .success {
                child.process.terminate(); child.process.waitUntilExit()
                preconditionFailure("owned diagnostic child did not reach its rotation barrier")
            }
            precondition(received.value == Data("ready\n".utf8))
        }
        let current = logs.appendingPathComponent("ble_diag.log")
        let descriptor = open(logs.appendingPathComponent(".ble_diag.lock").path, O_RDWR | O_NOFOLLOW | O_CLOEXEC)
        precondition(descriptor >= 0 && flock(descriptor, LOCK_EX | LOCK_NB) == 0)
        let before = try! Data(contentsOf: current)
        let started = DispatchTime.now().uptimeNanoseconds
        finish(launch("contended", tag: "locked"), seconds: 5)
        precondition(DispatchTime.now().uptimeNanoseconds - started < 5_000_000_000)
        precondition((try! Data(contentsOf: current)) == before,
                     "independent child wrote while parent held the OS lock")
        precondition(flock(descriptor, LOCK_UN) == 0); close(descriptor)
        finish(launch("append", tag: "released"))
        precondition((try! String(contentsOf: current, encoding: .utf8)).contains("child-released-probe"))
        let a = launch("rotate", tag: "A"), b = launch("rotate", tag: "B")
        ready(a); ready(b) // Both real writers have run their concurrent rollover workload.
        a.input.fileHandleForWriting.write(Data([1])); finish(a)
        b.input.fileHandleForWriting.write(Data([1])); finish(b)
        var retained = "", total = 0
        for name in BoundedBLEDiagnosticLog.fileNames {
            let path = logs.appendingPathComponent(name), bytes = try! Data(contentsOf: path)
            var metadata = stat(); precondition(lstat(path.path, &metadata) == 0)
            precondition(BoundedBLEDiagnosticLog.isPrivateRegularFile(metadata))
            precondition(bytes.count <= 65_536 && !bytes.isEmpty)
            total += bytes.count
            retained += String(data: bytes, encoding: .utf8)!
        }
        precondition(total <= 3 * 65_536 && retained.contains("END-child-A") && retained.contains("END-child-B"))
        precondition((try! Data(contentsOf: external)) == originalExternal
                     && (try! Data(contentsOf: unrelated)) == originalUnrelated)
        precondition(reaped.count == 4)
        print("REAPED BLE diagnostic children \(reaped.map(String.init).joined(separator: ", "))")
        print("PASS real processes: child drops under parent's lock; resumes after unlock; two writers rotate within 64 KiB x 3 and preserve unrelated targets")
    }

    static func unsafePaths() {
        let attacks = ["current-symlink", "archive1-symlink", "archive2-symlink", "current-hardlink",
                       "archive-hardlink", "lock-symlink", "lock-hardlink", "fifo", "directory-symlink",
                       "ancestor-symlink", "directory-mode", "file-mode", "oversized-existing"]
        for attack in attacks {
            let base = directory(attack)
            let outside = base.appendingPathComponent("outside")
            try! fm.createDirectory(at: outside, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            let target = outside.appendingPathComponent("external-target")
            file(target, bytes: Data("external bytes must not change".utf8))
            let targetBytes = try! Data(contentsOf: target)
            let logRoot = base.appendingPathComponent("logs")
            if attack == "directory-symlink" || attack == "ancestor-symlink" {
                try! fm.createSymbolicLink(at: logRoot, withDestinationURL: outside)
            } else {
                try! fm.createDirectory(at: logRoot, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            }
            var named = logRoot.appendingPathComponent("ble_diag.log")
            if attack.hasPrefix("archive1") { named = logRoot.appendingPathComponent("ble_diag.log.1") }
            if attack.hasPrefix("archive2") || attack == "archive-hardlink" { named = logRoot.appendingPathComponent("ble_diag.log.2") }
            if attack.hasPrefix("lock-") { named = logRoot.appendingPathComponent(".ble_diag.lock") }
            if attack.hasSuffix("symlink") && !attack.contains("directory") && !attack.contains("ancestor") {
                try! fm.createSymbolicLink(at: named, withDestinationURL: target)
            } else if attack.hasSuffix("hardlink") {
                try! fm.linkItem(at: target, to: named)
            } else if attack == "fifo" { precondition(mkfifo(named.path, 0o600) == 0) }
            else if attack == "directory-mode" { precondition(chmod(logRoot.path, 0o755) == 0) }
            else if attack == "file-mode" { file(named); precondition(chmod(named.path, 0o644) == 0) }
            else if attack == "oversized-existing" { file(named, bytes: Data(repeating: 1, count: 65_537)) }
            let destination = attack == "ancestor-symlink" ? logRoot.appendingPathComponent("nested") : logRoot
            let logger = BoundedBLEDiagnosticLog(directory: { destination })
            let beforeNames = try! fm.contentsOfDirectory(atPath: outside.path)
            for _ in 0..<40 { emit(logger, "unsafe path must drop") }
            precondition((try! Data(contentsOf: target)) == targetBytes, "\(attack) changed external bytes")
            precondition((try! fm.contentsOfDirectory(atPath: outside.path)) == beforeNames,
                         "\(attack) created/rotated a file outside its runtime root")
            if attack == "file-mode" { precondition((try! String(contentsOf: named, encoding: .utf8)) == "original") }
        }
        // Changing permissions after a successful write must stop future writes.
        let liveRoot = directory("changed-permissions"), path = liveRoot.appendingPathComponent("ble_diag.log")
        let logger = BoundedBLEDiagnosticLog(directory: { liveRoot })
        emit(logger)
        let before = try! Data(contentsOf: path)
        precondition(chmod(path.path, 0o644) == 0)
        emit(logger)
        precondition((try! Data(contentsOf: path)) == before)
        precondition(chmod(path.path, 0o600) == 0)
        emit(logger)
        precondition((try! Data(contentsOf: path)) != before)
        var info = stat(); precondition(lstat(path.path, &info) == 0)
        precondition(BoundedBLEDiagnosticLog.isPrivateRegularFile(info))
        info.st_uid = geteuid() &+ 1
        precondition(!BoundedBLEDiagnosticLog.isPrivateRegularFile(info), "production owner check accepted another UID")
        var directoryInfo = stat(); precondition(lstat(liveRoot.path, &directoryInfo) == 0)
        directoryInfo.st_uid = geteuid() &+ 1
        precondition(!BoundedBLEDiagnosticLog.isPrivateDirectory(directoryInfo))
        print("PASS production safety: 13 path/size attacks preserve external bytes; changed permissions and foreign UID reject")
    }

    static func main() {
        if let mode = ProcessInfo.processInfo.environment["BLE_DIAGNOSTIC_CHILD_MODE"] {
            childWriter(mode)
            return
        }
        queueAdmissionAndFailures()
        utf8Bounds()
        diskRotationAndContention()
        realProcesses()
        unsafePaths()
        print("PASS bounded BLE diagnostics; production writer compiled directly; no BLE devices, real user files or network")
    }
}
