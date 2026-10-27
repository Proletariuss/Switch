import Foundation
import Combine
import Darwin

private struct BackendShutdownQuarantine: Codable {
    let schemaVersion: Int
    let ownerPID: Int32
    let generation: UInt64
    let resource: String
    let session: String
    let reason: String
    let timestamp: Date
}

struct BackendExitInfo: Equatable {
    let status: Int32
    let signaled: Bool
    let expected: Bool
    let occurredAt: Date

    var summary: String { signaled ? "终止信号 \(status)" : "退出码 \(status)" }
}

enum BackendStopWaitReason: Equatable {
    case cancelled
    case timedOut
}

/// A completed wait proves only the owned process exited (or never existed).
/// A nonzero/signaled exit remains negative cleanup evidence.
enum BackendStopWaitResult: Equatable {
    case noOwnedChild
    case exited(BackendExitInfo)
    case pending(BackendStopWaitReason)

    var ownedProcessExitConfirmed: Bool {
        switch self {
        case .noOwnedChild, .exited: return true
        case .pending: return false
        }
    }
}

/// Production quit ordering shared with the command-line synthetic-child
/// harness. AppKit's delegate supplies only bridge-close and final-reply hooks.
@MainActor
final class BackendAppTermination {
    private var task: Task<Void, Never>?
    private(set) var completed = false
    private(set) var result: BackendStopWaitResult?
    private let gracePeriod: TimeInterval
    private let forcedExitPeriod: TimeInterval

    init(gracePeriod: TimeInterval = 8, forcedExitPeriod: TimeInterval = 2) {
        self.gracePeriod = gracePeriod
        self.forcedExitPeriod = forcedExitPeriod
    }

    func request(backend: BackendProcess, stopBridge: @escaping @MainActor () -> Void,
                 reply: @escaping @MainActor () -> Void) {
        guard task == nil, !completed else { return }
        // Fence even a no-child quit synchronously, before the task can yield
        // to a late UI/autostart request. Ordinary stop remains restartable.
        backend.beginApplicationTermination()
        task = Task { @MainActor in
            var result = await backend.stopAndWait(timeout: gracePeriod)
            if result == .pending(.timedOut), backend.forceTerminateQuarantinedChild() {
                result = await backend.stopAndWait(timeout: forcedExitPeriod)
            }
            self.result = result
            guard result.ownedProcessExitConfirmed else {
                // An interrupted wait is not an exit acknowledgement. Leave
                // the bridge alive and allow a later quit request to rejoin.
                self.task = nil
                return
            }
            self.completed = true
            stopBridge()
            reply()
            self.task = nil
        }
    }
}

/// Called on the existing serial log queue. Disk usage is limited even when a
/// child emits a single large chunk; the current file plus three archives
/// occupy at most 20 MiB by default. Archive files retain owner-only access.
enum BoundedBackendLog {
    static func append(_ data: Data, to url: URL, maximumBytes: Int = 5 * 1_024 * 1_024,
                       archives: Int = 3) throws {
        guard maximumBytes > 0, archives >= 0, !data.isEmpty else { return }
        let fm = FileManager.default
        let payload = data.count > maximumBytes ? Data(data.suffix(maximumBytes)) : data
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        if size > maximumBytes || size + payload.count > maximumBytes {
            if archives > 0 {
                for index in stride(from: archives, through: 1, by: -1) {
                    let target = url.appendingPathExtension(String(index))
                    if fm.fileExists(atPath: target.path) { try fm.removeItem(at: target) }
                    let source = index == 1 ? url : url.appendingPathExtension(String(index - 1))
                    if fm.fileExists(atPath: source.path) {
                        try fm.moveItem(at: source, to: target)
                        // Older installations can already contain an oversized
                        // log. Bound it at rotation rather than preserving it.
                        let handle = try FileHandle(forUpdating: target)
                        defer { try? handle.close() }
                        let end = try handle.seekToEnd()
                        if end > UInt64(maximumBytes) {
                            try handle.seek(toOffset: end - UInt64(maximumBytes))
                            let tail = try handle.read(upToCount: maximumBytes) ?? Data()
                            try handle.truncate(atOffset: 0)
                            try handle.seek(toOffset: 0)
                            try handle.write(contentsOf: tail)
                        }
                        try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
                    }
                }
            } else if fm.fileExists(atPath: url.path) {
                try fm.removeItem(at: url)
            }
        }
        if !fm.fileExists(atPath: url.path) {
            guard fm.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600]) else {
                throw CocoaError(.fileWriteUnknown)
            }
        }
        let handle = try FileHandle(forWritingTo: url)
        defer { try? handle.close() }
        try handle.seekToEnd()
        try handle.write(contentsOf: payload)
    }
}

/// The app and both child runtimes share one isolated data root. The override
/// also lets offline checks keep every generated file inside the Switch tree.
enum SwitchRuntimePaths {
    static func dataDirectory(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        fileManager: FileManager = .default
    ) -> URL {
        if let context = AcceptanceBootstrap.context {
            return URL(fileURLWithPath: context.dataRoot, isDirectory: true)
        }
        if let path = environment["SWITCH_DATA_DIR"], !path.isEmpty {
            precondition(path.hasPrefix("/"), "SWITCH_DATA_DIR must be an absolute path")
            return URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL
        }
        let applicationSupport = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? fileManager.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support", isDirectory: true)
        return applicationSupport.appendingPathComponent("Switch", isDirectory: true)
    }
}

/// Observe the same owned process through the existing finite recovery window:
/// 180 s reconciliation + 10 s metadata + 20 s scheduling/handoff allowance.
/// This budget never terminates a child or certifies physical cleanup.
@MainActor
struct BackendStartupObservation {
    static let budget: TimeInterval = 210
    var uptime: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    var check: (TimeInterval) async -> Bool = { await BackendReadiness.check(timeout: $0) }
    var pause: (TimeInterval) async throws -> Void = { seconds in
        try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }
}

/// 管理后端子进程的生命周期，并对外暴露 Unix socket 路径。
///
/// 始终启动单一 Rust 后端；设备就绪状态由各原生设备服务单独报告。
@MainActor
final class BackendProcess: ObservableObject {
    enum State: Equatable {
        case stopped
        case starting
        case running
        case stopping
        case failed(String)
    }

    static let shared = BackendProcess()

    @Published private(set) var state: State = .stopped
    @Published private(set) var childPID: pid_t = 0
    @Published private(set) var lastExit: BackendExitInfo?
    private var process: Process?
    private var stopRequested = false
    private var applicationTerminationRequested = false
    private var completedStopResult: BackendStopWaitResult?
    private struct StopWaiter {
        let continuation: CheckedContinuation<BackendStopWaitResult, Never>
        let deadline: Task<Void, Never>?
    }
    private var stopWaiters: [UUID: StopWaiter] = [:]
    private var startupTask: Task<Void, Never>?
    private var launchGeneration: UInt64 = 0
    private let executableURLOverride: URL?
    private let startupObservation: BackendStartupObservation
    private var stdoutTask: Task<Void, Never>?
    private var stderrTask: Task<Void, Never>?
    private let logWriteQueue = DispatchQueue(label: "switch.backend.log-writer")
    private let dataDirectory: URL
    private var terminationQuarantine: BackendShutdownQuarantine?
    private var quarantineURL: URL { dataDirectory.appendingPathComponent("backend-shutdown-quarantine-v1.json") }

    let socketPath: String
    let bleSocketPath: String
    let logURL: URL

    init(supportDirectory: URL? = nil, executableURL: URL? = nil,
         startupObservation: BackendStartupObservation? = nil) {
        self.startupObservation = startupObservation ?? BackendStartupObservation()
        executableURLOverride = executableURL
        let fm = FileManager.default
        let support = supportDirectory ?? SwitchRuntimePaths.dataDirectory(fileManager: fm)
        dataDirectory = support
        try? fm.createDirectory(at: support, withIntermediateDirectories: true)
        try? fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: support.path)
        socketPath = support.appendingPathComponent("switch.sock").path
        bleSocketPath = support.appendingPathComponent("estk-ble.sock").path
        logURL = support.appendingPathComponent("switch.log")
        if !fm.fileExists(atPath: logURL.path) {
            _ = fm.createFile(
                atPath: logURL.path,
                contents: nil,
                attributes: [.posixPermissions: 0o600])
        }
        try? fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: logURL.path)

        UnixSocketURLProtocol.socketPath = socketPath
    }

    var binaryURL: URL? {
        executableURLOverride ?? Bundle.main.url(
            forResource: "switchd-rs",
            withExtension: nil,
            subdirectory: "backend")
    }

    func start() {
        // A requested stop retains this exact child until its exit receipt.
        guard !applicationTerminationRequested, state != .stopping else { return }
        if FileManager.default.fileExists(atPath: quarantineURL.path) {
            state = .failed("后端处于安全隔离：上次退出未能确认物理资源清理；请人工核验后解除隔离")
            return
        }
        // A slow/unresponsive child may still own hardware after startup times
        // out. Never launch a second child or kill the first one to retry.
        if let process, process.isRunning {
            if case .failed = state { beginReadinessCheck(for: process) }
            return
        }
        if let process { recordExit(of: process) }
        startupTask?.cancel()
        startupTask = nil
        launchGeneration &+= 1
        completedStopResult = nil
        stopRequested = false
        guard let binary = binaryURL else {
            state = .failed("缺少内置后端程序")
            return
        }

        do {
            try validateSocketPathForLaunch()
        } catch {
            state = .failed("后端 socket 路径不安全：\(error.localizedDescription)")
            return
        }

        let proc = Process()
        proc.executableURL = binary
        // Start the proven serial/AT owner by default. DJI first-generation
        // modules expose a private USB composition that accepts AT on macOS
        // but does not answer the standard QMI CTL handshake. Explicit
        // `-backend=qmi` remains available for standard QMI compositions.
        proc.arguments = ["-listen", "unix:\(socketPath)", "-backend=at"]
        if let context = AcceptanceBootstrap.context {
            do {
                guard binary.path == context.backendPath else {
                    throw AcceptanceContext.Rejection.invalid("backend-path")
                }
                proc.arguments?.append(AcceptanceContext.argumentPrefix + (try context.createDaemonCapsule()))
            } catch {
                state = .failed("隔离验收授权失效；拒绝启动后端")
                return
            }
        }
        var environment = ProcessInfo.processInfo.environment
        environment["PROLETARIUS_APP_TOKEN"] = LocalBackendAuthentication.appToken
        environment["PROLETARIUS_BLE_SOCKET"] = bleSocketPath
        environment["SWITCH_DATA_DIR"] = dataDirectory.path
        proc.environment = environment

        let outPipe = Pipe()
        let errPipe = Pipe()
        proc.standardOutput = outPipe
        proc.standardError = errPipe
        proc.terminationHandler = { [weak self] p in
            Task { @MainActor in
                self?.recordExit(of: p)
            }
        }

        do {
            try proc.run()
        } catch {
            state = .failed("无法启动后端：\(error.localizedDescription)")
            return
        }

        let logURL = self.logURL
        let logQueue = self.logWriteQueue
        stdoutTask = Task.detached(priority: .utility) {
            Self.drain(
                outPipe.fileHandleForReading,
                to: .standardOutput,
                logURL: logURL,
                logWriteQueue: logQueue)
        }
        stderrTask = Task.detached(priority: .utility) {
            Self.drain(
                errPipe.fileHandleForReading,
                to: .standardError,
                logURL: logURL,
                logWriteQueue: logQueue)
        }

        process = proc
        childPID = proc.processIdentifier
        beginReadinessCheck(for: proc)
    }

    private func beginReadinessCheck(for proc: Process) {
        startupTask?.cancel()
        launchGeneration &+= 1
        let generation = launchGeneration
        state = .starting

        startupTask = Task { @MainActor [weak self] in
            guard let self else { return }
            defer {
                if self.launchGeneration == generation, self.process === proc {
                    self.startupTask = nil
                }
            }
            // One absolute deadline includes every health request and delay.
            let deadline = self.startupObservation.uptime() + BackendStartupObservation.budget
            while self.startupObservation.uptime() < deadline {
                guard !Task.isCancelled, self.launchGeneration == generation,
                      self.process === proc else { return }
                if !proc.isRunning {
                    self.state = .failed("后端进程提前退出，请查看日志")
                    return
                }
                let remaining = deadline - self.startupObservation.uptime()
                let ready = await self.startupObservation.check(min(1, remaining))
                guard !Task.isCancelled, self.launchGeneration == generation,
                      self.process === proc else { return }
                guard proc.isRunning else {
                    self.state = .failed("后端进程提前退出，请查看日志")
                    return
                }
                if ready, self.startupObservation.uptime() < deadline {
                    self.state = .running
                    return
                }
                let pause = min(0.1, deadline - self.startupObservation.uptime())
                if pause > 0 {
                    do { try await self.startupObservation.pause(pause) }
                    catch { return }
                }
            }
            guard !Task.isCancelled, self.launchGeneration == generation,
                  self.process === proc, self.state != .stopping else { return }
            self.state = .failed(proc.isRunning
                ? "后台启动或恢复核对结果仍未确认；进程继续保留，可再次检查"
                : "后端进程提前退出，请查看日志")
        }
    }

    /// Irreversible for this App lifetime: no UI/autostart callback may launch
    /// another child after quit begins, including after its exit is confirmed.
    func beginApplicationTermination() {
        applicationTerminationRequested = true
        if let proc = process, proc.isRunning {
            // Freeze the generation that will own this stop. A prior ordinary
            // stop already advanced it; a fresh App stop will advance it once.
            let quarantinedGeneration = stopRequested ? launchGeneration : launchGeneration &+ 1
            let evidence = BackendShutdownQuarantine(
                schemaVersion: 1, ownerPID: childPID, generation: quarantinedGeneration,
                resource: "backend-owned-physical-sessions",
                session: "\(childPID)-\(launchGeneration)",
                reason: "application termination requested; physical cleanup completion pending",
                timestamp: Date())
            do {
                let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .iso8601
                let data = try encoder.encode(evidence)
                try data.write(to: quarantineURL, options: [.atomic])
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: quarantineURL.path)
                terminationQuarantine = evidence
            } catch {
                state = .failed("无法持久化退出隔离证据：\(error.localizedDescription)")
                return
            }
        }
        stop()
    }

    /// May only kill the exact child whose durable quarantine was written.
    /// It never clears that quarantine or reports physical cleanup success.
    func forceTerminateQuarantinedChild() -> Bool {
        guard applicationTerminationRequested, let proc = process, proc.isRunning,
              let evidence = terminationQuarantine,
              evidence.ownerPID == childPID,
              evidence.generation == launchGeneration,
              FileManager.default.fileExists(atPath: quarantineURL.path) else { return false }
        return Darwin.kill(childPID, SIGKILL) == 0
    }

    /// Request shutdown once. Neither signaling nor a timeout is an exit
    /// receipt; retain the PID, process and log drains while it remains alive.
    func stop() {
        startupTask?.cancel()
        startupTask = nil
        guard let proc = process else {
            state = .stopped
            return
        }
        guard proc.isRunning else {
            recordExit(of: proc)
            return
        }
        state = .stopping
        guard !stopRequested else { return }
        launchGeneration &+= 1
        stopRequested = true
        proc.terminate()
    }

    /// Cancellation/deadline end only this wait. The owned stop and other
    /// waiters remain active, and start() continues to reject replacement.
    func stopAndWait(timeout: TimeInterval? = nil) async -> BackendStopWaitResult {
        stop()
        if let result = completedStopResult, process == nil { return result }
        guard process != nil else { return .noOwnedChild }
        if Task.isCancelled { return .pending(.cancelled) }
        if let timeout, !timeout.isFinite || timeout <= 0 { return .pending(.timedOut) }
        let identity = UUID()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                if Task.isCancelled {
                    continuation.resume(returning: .pending(.cancelled))
                    return
                }
                // Main-actor serialization prevents the termination callback
                // from interleaving between this check and waiter admission.
                if let result = self.completedStopResult, self.process == nil {
                    continuation.resume(returning: result)
                    return
                }
                let deadline = timeout.map { seconds in
                    Task { @MainActor [weak self] in
                        do { try await Task.sleep(for: .seconds(seconds)) }
                        catch { return }
                        self?.finishStopWaiter(identity, result: .pending(.timedOut))
                    }
                }
                self.stopWaiters[identity] = StopWaiter(continuation: continuation, deadline: deadline)
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                self?.finishStopWaiter(identity, result: .pending(.cancelled))
            }
        }
    }

    private func finishStopWaiter(_ identity: UUID, result: BackendStopWaitResult) {
        guard let waiter = stopWaiters.removeValue(forKey: identity) else { return }
        waiter.deadline?.cancel()
        waiter.continuation.resume(returning: result)
    }

    private func recordExit(of proc: Process) {
        guard process === proc, !proc.isRunning else { return }
        let wasStopping = stopRequested
        let priorState = state
        launchGeneration &+= 1
        startupTask?.cancel()
        startupTask = nil
        process = nil
        childPID = 0
        stopRequested = false
        let exit = BackendExitInfo(status: proc.terminationStatus,
            signaled: proc.terminationReason == .uncaughtSignal,
            expected: wasStopping, occurredAt: Date())
        lastExit = exit
        let receipt = BackendStopWaitResult.exited(exit)
        completedStopResult = receipt
        // Only the daemon's normal, requested exit is affirmative cleanup
        // evidence. Forced/signalled/nonzero exits retain the durable fence.
        if applicationTerminationRequested, wasStopping, exit.status == 0, !exit.signaled,
           terminationQuarantine != nil {
            do {
                try FileManager.default.removeItem(at: quarantineURL)
                terminationQuarantine = nil
            } catch {
                // Fail closed: a stale marker prevents the next daemon launch.
            }
        }
        if wasStopping {
            state = .stopped
        } else if priorState == .starting {
            state = .failed("后端进程提前退出（\(exit.summary)），可重试或导出诊断摘要")
        } else {
            state = .failed("后端进程意外退出（\(exit.summary)），可重试或导出诊断摘要")
        }
        // Reader/log tasks continue to EOF; do not cancel their final drains.
        for identity in Array(stopWaiters.keys) { finishStopWaiter(identity, result: receipt) }
    }

    /// Validate the file shape without connecting to or removing an existing
    /// socket. The Rust backend's root and listener leases own admission and
    /// stale-socket reclamation, including listeners with a full accept queue.
    private func validateSocketPathForLaunch() throws {
        let fileManager = FileManager.default
        if (try? fileManager.destinationOfSymbolicLink(atPath: socketPath)) != nil {
            throw NSError(
                domain: "Proletarius.BackendProcess",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "拒绝覆盖符号链接"])
        }
        guard fileManager.fileExists(atPath: socketPath) else { return }
        let attributes = try fileManager.attributesOfItem(atPath: socketPath)
        guard attributes[.type] as? FileAttributeType == .typeSocket else {
            throw NSError(
                domain: "Proletarius.BackendProcess",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "拒绝覆盖非 socket 文件"])
        }
    }

    nonisolated private static func drain(
        _ handle: FileHandle,
        to dest: FileHandle,
        logURL: URL,
        logWriteQueue: DispatchQueue
    ) {
        while true {
            let data = handle.availableData
            if data.isEmpty { break }
            try? dest.write(contentsOf: data)
            // stdout 与 stderr 分别在后台任务中读取，统一串行追加，避免日志互相覆盖。
            logWriteQueue.sync {
                try? BoundedBackendLog.append(data, to: logURL)
            }
        }
        try? handle.close()
    }
}

/// Startup health checks have their own deadline: APIClient's normal resource
/// budget is intentionally much longer, and a custom URLProtocol can be stuck
/// waiting for a peer that accepted a socket but never sent HTTP.
enum BackendReadiness {
    static func check(timeout: TimeInterval) async -> Bool {
        guard timeout > 0, !Task.isCancelled else { return false }
        let attempt = Attempt()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                attempt.start(timeout: timeout, path: "/api/v2/runtime", continuation: continuation)
            }
        } onCancel: {
            attempt.finish(false)
        }
    }

    private struct Health: Decodable { let ok: Bool }

    private final class Attempt: @unchecked Sendable {
        private let lock = NSLock()
        private var finished = false
        private var continuation: CheckedContinuation<Bool, Never>?
        private var task: URLSessionDataTask?
        private var session: URLSession?
        private var deadline: DispatchWorkItem?

        func start(timeout: TimeInterval, path: String,
                   continuation: CheckedContinuation<Bool, Never>) {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.protocolClasses = [UnixSocketURLProtocol.self]
            configuration.timeoutIntervalForRequest = timeout
            configuration.timeoutIntervalForResource = timeout
            let session = URLSession(configuration: configuration)
            // Service liveness is separate from device availability; an absent
            // module must not make a healthy native daemon fail its launch probe.
            var request = URLRequest(url: URL(string: "http+unix://proletarius\(path)")!)
            request.setValue(LocalBackendAuthentication.appToken,
                             forHTTPHeaderField: LocalBackendAuthentication.headerName)
            let task = session.dataTask(with: request) { [weak self] data, response, error in
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                let healthy = error == nil && (200..<300).contains(status)
                    && data.flatMap { try? JSONDecoder().decode(Health.self, from: $0) }?.ok == true
                self?.finish(healthy)
            }
            let deadline = DispatchWorkItem { [weak self] in self?.finish(false) }
            lock.lock()
            guard !finished else {
                lock.unlock()
                task.cancel()
                session.invalidateAndCancel()
                continuation.resume(returning: false)
                return
            }
            self.continuation = continuation
            self.task = task
            self.session = session
            self.deadline = deadline
            lock.unlock()
            DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + timeout, execute: deadline)
            task.resume()
        }

        func finish(_ healthy: Bool) {
            lock.lock()
            guard !finished else { lock.unlock(); return }
            finished = true
            let continuation = self.continuation
            let task = self.task
            let session = self.session
            let deadline = self.deadline
            self.continuation = nil
            self.task = nil
            self.session = nil
            self.deadline = nil
            lock.unlock()
            deadline?.cancel()
            task?.cancel()
            session?.invalidateAndCancel()
            continuation?.resume(returning: healthy)
        }
    }
}
