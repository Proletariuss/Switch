import Darwin
import Foundation

// This executable is also its own fake backend. All sockets, logs and control
// files live in its unique temporary directory; it never runs the app/backend.
private func require(_ condition: @autoclosure () -> Bool, _ message: String) {
    if !condition() { fatalError(message) }
}

// Awaited conditions are evaluated before this ordinary argument; they cannot
// be placed inside require()'s synchronous autoclosure.
private func requireValue(_ condition: Bool, _ message: String) {
    if !condition { fatalError(message) }
}

private func address(_ path: String) -> sockaddr_un {
    var value = sockaddr_un()
    value.sun_family = sa_family_t(AF_UNIX)
    require(path.utf8.count < MemoryLayout.size(ofValue: value.sun_path), "socket path too long")
    withUnsafeMutablePointer(to: &value.sun_path) { pointer in
        for (index, byte) in path.utf8.enumerated() {
            UnsafeMutableRawPointer(pointer).storeBytes(of: byte, toByteOffset: index, as: UInt8.self)
        }
    }
    return value
}

private func socketIdentity(_ path: String) -> String {
    var info = stat()
    require(Darwin.lstat(path, &info) == 0, "fixture socket disappeared")
    require((info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK), "fixture path is no longer a socket")
    return "\(info.st_dev):\(info.st_ino)"
}

// The parent owns these descriptors; exec children must not prolong a fixture
// listener or its queued clients after the parent closes them.
private func ownedSocket() -> Int32 {
    let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    require(descriptor >= 0, "fixture socket failed")
    require(fcntl(descriptor, F_SETFD, FD_CLOEXEC) == 0, "fixture close-on-exec failed")
    let flags = fcntl(descriptor, F_GETFL)
    require(flags >= 0 && fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) == 0,
            "fixture nonblocking socket failed")
    return descriptor
}

private func connectSocket(_ descriptor: Int32, _ path: String) -> Int32 {
    var socketAddress = address(path)
    return withUnsafePointer(to: &socketAddress) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
}

private final class ExistingSocket {
    let path: String
    let identity: String
    private var listener: Int32
    private var clients: [Int32] = []

    init(path: String, stale: Bool = false, full: Bool = false) {
        self.path = path
        let descriptor = ownedSocket()
        listener = descriptor
        var socketAddress = address(path)
        let bound = withUnsafePointer(to: &socketAddress) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        require(bound == 0, "existing fixture bind failed")
        identity = socketIdentity(path)
        if stale {
            Darwin.close(listener)
            listener = -1
        } else {
            require(Darwin.listen(listener, 1) == 0, "existing fixture listen failed")
            if full { fillQueue() }
        }
    }

    private func fillQueue() {
        var connected = 0
        var saturated = false
        for _ in 0..<64 {
            let client = ownedSocket()
            clients.append(client)
            let result = connectSocket(client, path)
            let connectError = errno
            if result == 0 { connected += 1; continue }
            if connectError == EINPROGRESS {
                var pending = pollfd(fd: client, events: Int16(POLLOUT), revents: 0)
                let ready = Darwin.poll(&pending, 1, 50)
                require(ready >= 0, "fixture pending connect poll failed")
                if ready == 0 { saturated = true; break }
                var socketError: Int32 = 0
                var size = socklen_t(MemoryLayout<Int32>.size)
                require(getsockopt(client, SOL_SOCKET, SO_ERROR, &socketError, &size) == 0,
                        "fixture pending connect status failed")
                if socketError == 0 { connected += 1; continue }
                require(socketError == ECONNREFUSED, "unexpected pending connect error")
            } else {
                require(connectError == EAGAIN || connectError == EWOULDBLOCK || connectError == ECONNREFUSED,
                        "unexpected full-queue connect error")
            }
            saturated = true
            break
        }
        require(connected > 0 && saturated, "fixture did not actually fill its listener queue")
    }

    func drainQueue() {
        guard listener >= 0 else { return }
        for _ in 0..<128 {
            let peer = Darwin.accept(listener, nil, nil)
            if peer < 0 {
                require(errno == EAGAIN || errno == EWOULDBLOCK, "fixture accept failed")
                break
            }
            Darwin.close(peer)
        }
        for client in clients { Darwin.close(client) }
        clients.removeAll()
    }

    func assertStillAccepts() {
        require(socketIdentity(path) == identity, "launch changed existing listener inode")
        drainQueue()
        let client = ownedSocket()
        defer { Darwin.close(client) }
        let result = connectSocket(client, path)
        require(result == 0 || errno == EINPROGRESS, "preserved listener no longer connects")
        var readable = pollfd(fd: listener, events: Int16(POLLIN), revents: 0)
        require(Darwin.poll(&readable, 1, 500) > 0 && (readable.revents & Int16(POLLIN)) != 0,
                "preserved listener no longer accepts")
        let peer = Darwin.accept(listener, nil, nil)
        require(peer >= 0, "preserved listener accept failed")
        Darwin.close(peer)
    }

    deinit {
        for client in clients { Darwin.close(client) }
        if listener >= 0 { Darwin.close(listener) }
        // Keep the generated socket node as evidence; no fixture unlinks it.
    }
}

private func fakeBackend(_ path: String) {
    signal(SIGPIPE, SIG_IGN)
    let originalParent = getppid()
    DispatchQueue.global().async {
        while true {
            usleep(50_000)
            if getppid() != originalParent { exit(0) }
        }
    }
    let modePath = URL(fileURLWithPath: path).deletingLastPathComponent().appendingPathComponent("mode")
    require(ProcessInfo.processInfo.environment["SWITCH_DATA_DIR"] == modePath.deletingLastPathComponent().path,
            "child backend did not inherit its owner's isolated data directory")
    let mode = (try? String(contentsOf: modePath, encoding: .utf8)) ?? "healthy"
    if mode == "exit" { return }
    if mode == "owned-idle" { while true { pause() } }
    if mode == "exit-failure" { exit(7) }
    if mode == "reject-existing" {
        // Simulate a child rejecting admission, not Rust socket reclamation.
        // It must observe the parent's original node without touching it.
        try! socketIdentity(path).write(
            to: modePath.deletingLastPathComponent().appendingPathComponent("observed-existing"),
            atomically: true, encoding: .utf8)
        return
    }
    if mode == "late" { signal(SIGTERM, SIG_IGN) }
    var stopSignal: DispatchSourceSignal?
    var releaseSignal: DispatchSourceSignal?
    if mode == "gated-stop" || mode == "gated-stop-failure" || mode == "gated-startup" {
        // The selected fake child acknowledges SIGTERM but remains alive
        // until its owner explicitly releases it. No physical device is used.
        signal(SIGTERM, SIG_IGN)
        signal(SIGUSR1, SIG_IGN)
        let requested = modePath.deletingLastPathComponent().appendingPathComponent("stop-requested")
        let stop = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global())
        stop.setEventHandler {
            let count = Int((try? String(contentsOf: requested, encoding: .utf8)) ?? "0") ?? 0
            try! String(count + 1).write(to: requested, atomically: true, encoding: .utf8)
        }
        let release = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .global())
        release.setEventHandler { exit(mode == "gated-stop-failure" ? 7 : 0) }
        stop.resume()
        release.resume()
        stopSignal = stop
        releaseSignal = release
    }
    defer { stopSignal?.cancel(); releaseSignal?.cancel() }
    let listener = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    require(listener >= 0, "fixture socket failed")
    defer { Darwin.close(listener) }
    var socketAddress = address(path)
    let bound = withUnsafePointer(to: &socketAddress) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    require(bound == 0, "fixture bind failed")
    if mode == "bound" {
        while true { pause() }
    }
    require(Darwin.listen(listener, 16) == 0, "fixture listen failed")
    while true {
        let client = Darwin.accept(listener, nil, nil)
        if client < 0 { return }
        var bytes = [UInt8](repeating: 0, count: 4096)
        let count = Darwin.read(client, &bytes, bytes.count)
        let request = count > 0 ? String(decoding: bytes.prefix(count), as: UTF8.self) : ""
        let requestMode = (try? String(contentsOf: modePath, encoding: .utf8)) ?? mode
        let token = ProcessInfo.processInfo.environment["PROLETARIUS_APP_TOKEN"] ?? "missing"
        let authorized = request.lowercased().contains("x-proletarius-app-token: " + token.lowercased())
        let expectedPath = "/api/v2/runtime "
        require(request.hasPrefix("GET \(expectedPath)"), "probe used the wrong backend readiness route")
        // Keep accepted peers open without sending or closing. This catches
        // cancellation that returns to the UI but leaves the protocol reader blocked.
        if requestMode == "silent" || requestMode == "gated-startup" {
            try! "accepted".write(to: modePath.deletingLastPathComponent().appendingPathComponent("accepted"),
                                  atomically: true, encoding: .utf8)
            continue
        }
        if requestMode == "late" { usleep(400_000) }
        if requestMode == "exit-after-request" { Darwin.close(client); return }
        let status = !authorized || requestMode == "unauthorized" ? 401 : 200
        let body: String
        switch requestMode {
        case "malformed": body = "not-json"
        case "false": body = "{\"ok\":false}"
        case "go-starting": body = "{\"ok\":true,\"backend\":\"rust\",\"go_backend_connected\":false}"
        default: body = "{\"ok\":true}"
        }
        let response = "HTTP/1.1 \(status) Fixture\r\nContent-Length: \(body.utf8.count)\r\nConnection: close\r\n\r\n\(body)"
        response.withCString { pointer in _ = Darwin.write(client, pointer, response.utf8.count) }
        Darwin.close(client)
    }
}

@MainActor
private final class Fixture {
    let directory: URL
    let backend: BackendProcess

    init(root: URL, name: String, mode: String,
         observation: BackendStartupObservation? = nil) throws {
        directory = root.appendingPathComponent(name)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try mode.write(to: directory.appendingPathComponent("mode"), atomically: true, encoding: .utf8)
        backend = BackendProcess(supportDirectory: directory,
                                 executableURL: URL(fileURLWithPath: CommandLine.arguments[0]),
                                 startupObservation: observation)
    }

    func start() { backend.start() }

    func releaseStoppedChild() {
        let pid = backend.childPID
        require(pid > 0 && Darwin.kill(pid, SIGUSR1) == 0, "could not release owned synthetic child")
    }

    func cleanup() async {
        let pid = backend.childPID
        let result = await backend.stopAndWait(timeout: 2)
        // Only the deliberately SIGTERM-ignoring old fixture needs failure
        // cleanup. Never signal a retired PID or another process generation.
        if !result.ownedProcessExitConfirmed && pid > 0 && backend.childPID == pid {
            _ = Darwin.kill(pid, SIGKILL)
        }
        let exited = await waitUntil { self.backend.childPID == 0 }
        require(exited, "owned fixture cleanup did not receive actual child exit")
    }
}

@MainActor
private func waitUntil(seconds: TimeInterval = 2, _ condition: () -> Bool) async -> Bool {
    let deadline = ProcessInfo.processInfo.systemUptime + seconds
    while !condition(), ProcessInfo.processInfo.systemUptime < deadline {
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    return condition()
}

private func threadCount() -> Int {
    var threads: thread_act_array_t?
    var count: mach_msg_type_number_t = 0
    require(task_threads(mach_task_self_, &threads, &count) == KERN_SUCCESS,
            "failed to count this task's threads")
    guard let threads else { fatalError("task_threads returned no thread array") }
    defer {
        let bytes = vm_size_t(Int(count) * MemoryLayout<thread_act_t>.stride)
        require(vm_deallocate(mach_task_self_, vm_address_t(UInt(bitPattern: threads)), bytes) == KERN_SUCCESS,
                "failed to deallocate this task's thread array")
    }
    for index in 0..<Int(count) {
        require(mach_port_deallocate(mach_task_self_, threads[index]) == KERN_SUCCESS,
                "failed to deallocate this task's thread port")
    }
    return Int(count)
}

private func fileDescriptorCount() -> Int {
    // Query only this process's descriptor table. Opening /dev/fd would need
    // device-directory access that the hardware-denying sandbox forbids.
    let limit = Darwin.getdtablesize()
    require(limit > 0, "invalid own descriptor-table limit")
    var count = 0
    for descriptor in 0..<limit {
        if Darwin.fcntl(descriptor, F_GETFD) >= 0 {
            count += 1
        } else {
            let failure = errno
            require(failure == EBADF, "own descriptor query failed with errno \(failure)")
        }
    }
    return count
}

@MainActor
private func checkSocketAdmission(root: URL) async throws {
    for kind in ["live", "stale", "full"] {
        let fixture = try Fixture(root: root, name: "admit-\(kind)", mode: "reject-existing")
        let socket = ExistingSocket(path: fixture.backend.socketPath, stale: kind == "stale", full: kind == "full")
        let observed = fixture.directory.appendingPathComponent("observed-existing")
        fixture.start()
        let childObserved = await waitUntil { FileManager.default.fileExists(atPath: observed.path) }
        require(childObserved, "Swift did not leave the \(kind) socket for child admission")
        let childIdentity = try String(contentsOf: observed, encoding: .utf8)
        require(childIdentity == socket.identity, "child saw a replaced \(kind) socket")
        require(socketIdentity(fixture.backend.socketPath) == socket.identity,
                "Swift removed the \(kind) socket")
        // Drain only after the child observed the full queue. This releases
        // any concurrent readiness connect without admitting a new listener.
        socket.drainQueue()
        let childExited = await waitUntil { fixture.backend.childPID == 0 }
        require(childExited, "reject-existing child did not exit safely")
        require(fixture.backend.lastExit?.status == 0 && fixture.backend.lastExit?.signaled == false,
                "reject-existing child required termination")
        fixture.backend.stop()
        require(socketIdentity(fixture.backend.socketPath) == socket.identity,
                "child exit removed the \(kind) socket")
        if kind != "stale" { socket.assertStillAccepts() }
        await fixture.cleanup()
    }

    // A second object with the same support directory must reach child
    // admission without probing/unlinking the first object's live listener.
    let first = try Fixture(root: root, name: "admit-pair", mode: "healthy")
    first.start()
    let firstReady = await waitUntil { first.backend.state == .running }
    require(firstReady, "first admission fixture did not become ready")
    let firstPID = first.backend.childPID
    let firstIdentity = socketIdentity(first.backend.socketPath)
    let second = try Fixture(root: root, name: "admit-pair", mode: "reject-existing")
    let observed = second.directory.appendingPathComponent("observed-existing")
    second.start()
    let secondObserved = await waitUntil { FileManager.default.fileExists(atPath: observed.path) }
    require(secondObserved, "second BackendProcess rejected before child admission")
    let secondIdentity = try String(contentsOf: observed, encoding: .utf8)
    require(secondIdentity == firstIdentity && socketIdentity(first.backend.socketPath) == firstIdentity,
            "second BackendProcess replaced the first listener")
    let secondExited = await waitUntil { second.backend.childPID == 0 }
    require(secondExited, "second admission child did not exit safely")
    require(second.backend.lastExit?.status == 0 && second.backend.lastExit?.signaled == false,
            "second admission child required termination")
    require(first.backend.childPID == firstPID && Darwin.kill(firstPID, 0) == 0,
            "second BackendProcess stopped the first child")
    let firstStillReady = await BackendReadiness.check(timeout: 1)
    require(firstStillReady && socketIdentity(first.backend.socketPath) == firstIdentity,
            "first listener lost authenticated readiness after second launch")
    await second.cleanup()
    await first.cleanup()

    for kind in ["file", "symlink"] {
        let fixture = try Fixture(root: root, name: "admit-\(kind)", mode: "reject-existing")
        let evidence = Data("preserve socket-path evidence".utf8)
        let target = fixture.directory.appendingPathComponent("evidence")
        if kind == "file" {
            try evidence.write(to: URL(fileURLWithPath: fixture.backend.socketPath))
        } else {
            try evidence.write(to: target)
            try FileManager.default.createSymbolicLink(atPath: fixture.backend.socketPath,
                                                       withDestinationPath: target.path)
        }
        fixture.start()
        require(fixture.backend.childPID == 0, "unsafe \(kind) socket path launched a child")
        if case .failed = fixture.backend.state {} else { fatalError("unsafe path was not rejected") }
        let preserved = try Data(contentsOf: URL(fileURLWithPath: fixture.backend.socketPath))
        require(preserved == evidence, "unsafe \(kind) path evidence changed")
        if kind == "symlink" {
            let destination = try FileManager.default.destinationOfSymbolicLink(atPath: fixture.backend.socketPath)
            require(destination == target.path, "unsafe symlink was replaced")
        }
        await fixture.cleanup()
    }
    print("PASS Swift admission preserves live/stale/full-queue socket inodes and first owner; unsafe shapes rejected (fake child only)")
}

@MainActor
private func checkStopAndAppTermination(root: URL) async throws {
    let held = try Fixture(root: root, name: "quit-held", mode: "gated-stop")
    held.start()
    requireValue(await waitUntil { held.backend.state == .running }, "quit fixture did not become ready")
    let pid = held.backend.childPID
    let coordinator = BackendAppTermination()
    var bridgeCloses = 0
    var quitReplies = 0
    let closeBridge = { @MainActor in
        require(held.backend.childPID == 0 && held.backend.state == .stopped,
                "bridge closed before actual owned child exit")
        bridgeCloses += 1
    }
    let reply = { @MainActor in
        require(bridgeCloses == 1, "quit reply preceded bridge shutdown")
        quitReplies += 1
    }
    coordinator.request(backend: held.backend, stopBridge: closeBridge, reply: reply)
    coordinator.request(backend: held.backend, stopBridge: closeBridge, reply: reply)
    let requested = held.directory.appendingPathComponent("stop-requested")
    requireValue(await waitUntil { FileManager.default.fileExists(atPath: requested.path) },
            "owned synthetic child did not receive stop request")
    require(held.backend.state == .stopping && held.backend.childPID == pid,
            "stopping discarded the exact owned PID")
    require(bridgeCloses == 0 && quitReplies == 0 && !coordinator.completed,
            "pending child was mistaken for a completed quit")
    held.backend.stop()
    held.backend.stop()
    held.start()
    require(held.backend.childPID == pid && held.backend.state == .stopping,
            "start replaced a still-stopping owned child")
    let timed = await held.backend.stopAndWait(timeout: 0.03)
    require(timed == .pending(.timedOut), "wait timeout fabricated an exit")
    var cancellationWaitEntered = false
    let cancelled = Task { @MainActor in
        cancellationWaitEntered = true
        return await held.backend.stopAndWait(timeout: 2)
    }
    // The actor cannot observe this flag until the waiter has suspended in
    // stopAndWait, so cancellation exercises an admitted continuation.
    requireValue(await waitUntil { cancellationWaitEntered }, "cancellable waiter never entered")
    cancelled.cancel()
    requireValue(await cancelled.value == .pending(.cancelled), "cancelled wait fabricated an exit")
    require(held.backend.childPID == pid && bridgeCloses == 0 && quitReplies == 0,
            "wait cancellation changed actual child/bridge lifetime")
    let stopCount = try String(contentsOf: requested, encoding: .utf8)
    require(stopCount == "1", "duplicate stop/quit sent another termination request")
    var mainActorServiced = false
    Task { @MainActor in mainActorServiced = true }
    requireValue(await waitUntil { mainActorServiced }, "quit wait blocked the main actor needed by BLE")
    held.releaseStoppedChild()
    requireValue(await waitUntil { coordinator.completed }, "actual child exit did not finish deferred quit")
    guard let result = coordinator.result, case .exited(let exit) = result else {
        fatalError("quit has no actual exit receipt")
    }
    require(exit.status == 0 && !exit.signaled && exit.expected,
            "successful synthetic child exit was not preserved")
    require(bridgeCloses == 1 && quitReplies == 1, "duplicate quit closed/replied more than once")
    require(Darwin.kill(pid, 0) != 0 && errno == ESRCH,
            "quit completed while the original OS child still existed")
    coordinator.request(backend: held.backend, stopBridge: closeBridge, reply: reply)
    let repeated = await held.backend.stopAndWait()
    require(repeated == .exited(exit) && quitReplies == 1 && bridgeCloses == 1,
            "completed stop/quit did not reuse the original receipt")
    held.start()
    require(held.backend.childPID == 0 && held.backend.lastExit == exit,
            "late autostart escaped the completed App quit admission fence")
    await held.cleanup()
    print("PASS exact owned exit gates bridge/reply; repeated quit/stop, timeout and cancellation remain fenced")

    let forced = try Fixture(root: root, name: "quit-forced-quarantine", mode: "gated-stop")
    forced.start()
    requireValue(await waitUntil { forced.backend.state == .running }, "forced fixture not ready")
    let forcedPID = forced.backend.childPID
    let forcedQuit = BackendAppTermination(gracePeriod: 0.03, forcedExitPeriod: 2)
    forcedQuit.request(backend: forced.backend, stopBridge: {}, reply: {})
    requireValue(await waitUntil { forcedQuit.completed }, "bounded quit did not force an unresponsive child")
    guard case .exited(let forcedExit) = forcedQuit.result else { fatalError("forced exit receipt missing") }
    require(forcedExit.signaled && forcedExit.status == SIGKILL, "forced quit was reported as normal cleanup")
    require(Darwin.kill(forcedPID, 0) != 0 && errno == ESRCH, "forced child still exists")
    let marker = forced.directory.appendingPathComponent("backend-shutdown-quarantine-v1.json")
    require(FileManager.default.fileExists(atPath: marker.path), "unknown physical cleanup lost quarantine")
    let fenced = BackendProcess(supportDirectory: forced.directory,
        executableURL: URL(fileURLWithPath: CommandLine.arguments[0]))
    fenced.start()
    require(fenced.childPID == 0, "successor escaped persisted shutdown quarantine")
    await forced.cleanup()
    print("PASS bounded quit retains exact quarantine and fences successor after forced exit")

    let overlapping = try Fixture(root: root, name: "stop-app-quit", mode: "gated-stop")
    overlapping.start()
    requireValue(await waitUntil { overlapping.backend.state == .running }, "overlap fixture not ready")
    let overlapPID = overlapping.backend.childPID
    overlapping.backend.stop()
    requireValue(await waitUntil {
        FileManager.default.fileExists(atPath: overlapping.directory.appendingPathComponent("stop-requested").path)
    }, "ordinary stop did not enter its gate")
    let overlapQuit = BackendAppTermination(gracePeriod: 0.03, forcedExitPeriod: 2)
    overlapQuit.request(backend: overlapping.backend, stopBridge: {}, reply: {})
    requireValue(await waitUntil { overlapQuit.completed },
                 "App quit could not join and force an ordinary stop already in flight")
    guard case .exited(let overlapExit) = overlapQuit.result else { fatalError("overlap exit receipt missing") }
    require(overlapExit.signaled && overlapExit.status == SIGKILL,
            "overlap force was relabeled as normal cleanup")
    require(Darwin.kill(overlapPID, 0) != 0 && errno == ESRCH, "overlap child still exists")
    let overlapMarker = overlapping.directory.appendingPathComponent("backend-shutdown-quarantine-v1.json")
    require(FileManager.default.fileExists(atPath: overlapMarker.path),
            "overlap unknown cleanup lost quarantine")
    let overlapSuccessor = BackendProcess(supportDirectory: overlapping.directory,
        executableURL: URL(fileURLWithPath: CommandLine.arguments[0]))
    overlapSuccessor.start()
    require(overlapSuccessor.childPID == 0, "overlap successor escaped quarantine")
    await overlapping.cleanup()
    print("PASS ordinary stop in flight joins bounded App quit with exact generation quarantine")

    let starting = try Fixture(root: root, name: "quit-starting", mode: "gated-startup")
    starting.start()
    requireValue(await waitUntil {
        FileManager.default.fileExists(atPath: starting.directory.appendingPathComponent("accepted").path)
    }, "startup fixture did not actually enter pending readiness")
    require(starting.backend.state == .starting, "fixture missed startup state")
    let startingPID = starting.backend.childPID
    // The child is alive and its first readiness request remains unanswered.
    let startupStop = Task { @MainActor in await starting.backend.stopAndWait(timeout: 2) }
    requireValue(await waitUntil { starting.backend.state == .stopping }, "startup quit did not stop readiness")
    requireValue(await waitUntil {
        FileManager.default.fileExists(atPath: starting.directory.appendingPathComponent("stop-requested").path)
    }, "startup child did not enter stop gate")
    starting.start()
    require(starting.backend.childPID == startingPID && starting.backend.state == .stopping,
            "late startup or retry escaped stop fence")
    starting.releaseStoppedChild()
    guard case .exited(let startupExit) = await startupStop.value else { fatalError("startup quit has no exit") }
    require(startupExit.expected && starting.backend.state == .stopped,
            "startup quit did not preserve expected actual exit")
    await starting.cleanup()
    print("PASS startup quit cancels readiness and retains child until exit")

    let failure = try Fixture(root: root, name: "quit-failure", mode: "gated-stop-failure")
    failure.start()
    requireValue(await waitUntil { failure.backend.state == .running }, "failure fixture not ready")
    let failedStop = Task { @MainActor in await failure.backend.stopAndWait(timeout: 2) }
    requireValue(await waitUntil {
        FileManager.default.fileExists(atPath: failure.directory.appendingPathComponent("stop-requested").path)
    }, "failure fixture did not receive stop")
    failure.releaseStoppedChild()
    guard case .exited(let failedExit) = await failedStop.value else { fatalError("failure exit missing") }
    require(failedExit.status == 7 && !failedExit.signaled && failedExit.expected,
            "nonzero stop result was changed into successful cleanup")
    require(failure.backend.lastExit == failedExit, "negative exit evidence was lost")
    await failure.cleanup()

    let gone = try Fixture(root: root, name: "already-exited", mode: "exit-failure")
    gone.start()
    requireValue(await waitUntil { gone.backend.childPID == 0 }, "exiting fixture did not exit")
    guard case .exited(let oldExit) = await gone.backend.stopAndWait() else { fatalError("actual prior exit lost") }
    require(oldExit.status == 7 && !oldExit.expected, "unexpected exit was relabeled as expected")
    let goneQuit = BackendAppTermination()
    var goneReplies = 0
    goneQuit.request(backend: gone.backend, stopBridge: {}, reply: { goneReplies += 1 })
    gone.start()
    require(gone.backend.childPID == 0, "quit of an already-exited child admitted immediate restart")
    requireValue(await waitUntil { goneQuit.completed }, "already-exited quit did not complete")
    gone.start()
    require(goneReplies == 1 && gone.backend.childPID == 0 && gone.backend.lastExit == oldExit,
            "late start after already-exited quit lost admission/exit evidence")
    await gone.cleanup()

    let missingRoot = root.appendingPathComponent("missing-binary")
    let missing = BackendProcess(supportDirectory: missingRoot,
        executableURL: missingRoot.appendingPathComponent("does-not-exist"))
    missing.start()
    if case .failed = missing.state {} else { fatalError("launch failure was not reported") }
    require(missing.childPID == 0, "failed launch published an owned PID")
    requireValue(await missing.stopAndWait() == .noOwnedChild, "failed launch invented an exit")
    let empty = BackendProcess(supportDirectory: root.appendingPathComponent("never-started"),
        executableURL: URL(fileURLWithPath: CommandLine.arguments[0]))
    let emptyQuit = BackendAppTermination()
    var emptyOrder: [String] = []
    emptyQuit.request(backend: empty, stopBridge: { emptyOrder.append("bridge") },
                      reply: { emptyOrder.append("reply") })
    empty.start()
    require(empty.childPID == 0, "no-child quit did not synchronously close start admission")
    emptyQuit.request(backend: empty, stopBridge: { emptyOrder.append("duplicate") },
                      reply: { emptyOrder.append("duplicate") })
    requireValue(await waitUntil { emptyQuit.completed }, "no-child quit did not complete")
    require(emptyQuit.result == .noOwnedChild && emptyOrder == ["bridge", "reply"],
            "no-child quit ordering/receipt was wrong")
    empty.start()
    require(empty.childPID == 0 && empty.state == .stopped,
            "late start escaped a completed no-child quit")
    print("PASS negative/already-exited/launch-failed/no-child quit evidence and permanent App start fence")

    // A normal stop does not express App termination. Prove a new actual
    // process can be spawned only after the previous owned exit is confirmed.
    let restart = try Fixture(root: root, name: "ordinary-restart", mode: "gated-stop")
    restart.start()
    requireValue(await waitUntil { restart.backend.state == .running }, "restart fixture not ready")
    let oldPID = restart.backend.childPID
    let normalStop = Task { @MainActor in await restart.backend.stopAndWait(timeout: 2) }
    requireValue(await waitUntil {
        FileManager.default.fileExists(atPath: restart.directory.appendingPathComponent("stop-requested").path)
    }, "normal stop did not reach owned child")
    restart.start()
    require(restart.backend.childPID == oldPID && restart.backend.state == .stopping,
            "normal restart overlapped a still-alive owned child")
    restart.releaseStoppedChild()
    guard case .exited(let normalExit) = await normalStop.value else { fatalError("normal stop missing exit") }
    require(normalExit.expected && normalExit.status == 0 && !normalExit.signaled,
            "normal stop did not preserve actual successful exit")
    // This second child exits before binding. This checks spawn admission,
    // not Rust stale-socket reclamation (covered by separate daemon tests).
    try "exit".write(to: restart.directory.appendingPathComponent("mode"), atomically: true, encoding: .utf8)
    restart.start()
    let newPID = restart.backend.childPID
    require(newPID > 0 && newPID != oldPID, "ordinary confirmed stop permanently disabled restart")
    requireValue(await waitUntil { restart.backend.childPID == 0 }, "restarted actual child did not exit")
    require(restart.backend.lastExit?.status == 0 && restart.backend.lastExit?.expected == false,
            "new process generation inherited the old expected-stop receipt")
    await restart.cleanup()
    print("PASS ordinary stop retains overlap fence and permits actual restart after confirmed exit")
}

@MainActor
private final class ObservationClock {
    var now: TimeInterval = 0
    private(set) var attempts: [TimeInterval] = []
    private var pending: [CheckedContinuation<Bool, Never>?] = []

    var observation: BackendStartupObservation {
        BackendStartupObservation(
            uptime: { self.now },
            check: { timeout in
                self.attempts.append(timeout)
                return await withCheckedContinuation { self.pending.append($0) }
            },
            pause: { _ in await Task.yield() })
    }

    func finish(_ index: Int, at time: TimeInterval, ready: Bool) {
        require(time >= now, "synthetic uptime moved backwards")
        now = time
        guard let continuation = pending[index] else { fatalError("probe completed twice") }
        pending[index] = nil
        continuation.resume(returning: ready)
    }
}

@MainActor
private func checkObservationBudget(root: URL) async throws {
    require(BackendStartupObservation.budget == 210, "finite recovery observation budget changed")
    for readyAt in [TimeInterval(6), TimeInterval(200)] {
        let clock = ObservationClock()
        let fixture = try Fixture(root: root, name: "observed-\(Int(readyAt))",
                                  mode: "owned-idle", observation: clock.observation)
        fixture.start()
        let pid = fixture.backend.childPID
        requireValue(await waitUntil { clock.attempts.count == 1 }, "controlled probe did not start")
        require(fixture.backend.state == .starting, "live startup was reported as failed before its budget")
        clock.finish(0, at: readyAt, ready: false)
        requireValue(await waitUntil { clock.attempts.count == 2 }, "slow startup was cut off by the old five-second budget")
        require(fixture.backend.state == .starting, "finite recovery observation did not remain starting")
        clock.finish(1, at: readyAt, ready: true)
        requireValue(await waitUntil { fixture.backend.state == .running }, "finite slow startup did not become ready")
        require(fixture.backend.childPID == pid && pid > 0, "slow readiness replaced its owned process")
        require(clock.attempts == [1, 1], "healthy result caused another observation request")
        await fixture.cleanup()
    }

    let expired = ObservationClock()
    let timeout = try Fixture(root: root, name: "observed-timeout", mode: "owned-idle",
                              observation: expired.observation)
    timeout.start()
    let retainedPID = timeout.backend.childPID
    requireValue(await waitUntil { expired.attempts.count == 1 }, "timeout probe missing")
    expired.finish(0, at: 210, ready: true)
    requireValue(await waitUntil {
        if case .failed = timeout.backend.state { return true }; return false
    }, "late successful probe escaped the absolute observation deadline")
    guard case .failed(let message) = timeout.backend.state else { fatalError("timeout state lost") }
    require(message.contains("未确认") && !message.contains("已退出"), "timeout claimed an actual process exit")
    require(timeout.backend.childPID == retainedPID && Darwin.kill(retainedPID, 0) == 0
            && timeout.backend.lastExit == nil, "deadline destroyed the live owner or invented an exit")
    await Task.yield()
    require(expired.attempts.count == 1, "deadline automatically retried observation")
    await timeout.cleanup()

    let replacementClock = ObservationClock()
    let replacement = try Fixture(root: root, name: "observed-replacement", mode: "owned-idle",
                                  observation: replacementClock.observation)
    replacement.start()
    let retiredPID = replacement.backend.childPID
    requireValue(await waitUntil { replacementClock.attempts.count == 1 }, "old controlled probe missing")
    let stopped = await replacement.backend.stopAndWait(timeout: 2)
    require(stopped.ownedProcessExitConfirmed, "old actual process did not exit")
    replacement.start()
    let currentPID = replacement.backend.childPID
    require(currentPID > 0 && currentPID != retiredPID, "actual replacement did not start")
    requireValue(await waitUntil { replacementClock.attempts.count == 2 }, "new controlled probe missing")
    replacementClock.finish(0, at: 6, ready: true)
    for _ in 0..<10 { await Task.yield() }
    require(replacement.backend.childPID == currentPID && replacement.backend.state == .starting,
            "old successful probe marked the new process ready")
    replacementClock.finish(1, at: 200, ready: true)
    requireValue(await waitUntil { replacement.backend.state == .running }, "current process readiness was lost")
    require(replacement.backend.childPID == currentPID, "current successful probe replaced its owner")
    await replacement.cleanup()
    print("PASS controlled 6/200-second readiness, 210-second owner-preserving deadline and stale generation fence")
}

@main
private struct BackendReadinessTests {
    @MainActor
    static func main() async throws {
        if CommandLine.arguments.count >= 3, CommandLine.arguments[1] == "-listen" {
            require(CommandLine.arguments.count == 4 && CommandLine.arguments[3] == "-backend=at",
                    "Rust backend launch must default to the proven AT owner")
            fakeBackend(String(CommandLine.arguments[2].dropFirst("unix:".count)))
            return
        }
        let root = URL(fileURLWithPath: CommandLine.arguments[1])
        try await checkObservationBudget(root: root)
        try await checkSocketAdmission(root: root)
        let rotating = root.appendingPathComponent("bounded.log")
        try Data(repeating: 88, count: 200).write(to: rotating)
        for index in 0..<20 {
            try BoundedBackendLog.append(Data(repeating: UInt8(index), count: 30), to: rotating,
                                         maximumBytes: 64, archives: 2)
        }
        for file in [rotating, rotating.appendingPathExtension("1"), rotating.appendingPathExtension("2")] {
            let attrs = try FileManager.default.attributesOfItem(atPath: file.path)
            require((attrs[.size] as? Int ?? 0) <= 64, "log archive exceeded configured limit")
            require((attrs[.posixPermissions] as? Int ?? 0) == 0o600, "log archive lost owner-only permissions")
        }
        require(!FileManager.default.fileExists(atPath: rotating.appendingPathExtension("3").path),
                "log archive count exceeded configured limit")
        try BoundedBackendLog.append(Data(repeating: 66, count: 500), to: rotating, maximumBytes: 64, archives: 2)
        let boundedData = try Data(contentsOf: rotating)
        require(boundedData.count == 64, "oversized single log chunk was not bounded")
        print("PASS bounded log rotation, oversized existing log/chunk, archive permissions")
        let isolated = SwitchRuntimePaths.dataDirectory(environment: ["SWITCH_DATA_DIR": root.path])
        require(isolated.path == root.standardizedFileURL.path, "data-root override was not honored")
        let defaultRoot = SwitchRuntimePaths.dataDirectory(environment: [:])
        require(defaultRoot.lastPathComponent == "Switch", "default data root must use the new app identity")
        let healthy = try Fixture(root: root, name: "healthy", mode: "healthy")
        require(healthy.backend.socketPath == healthy.directory.appendingPathComponent("switch.sock").path,
                "backend socket escaped its Switch fixture")
        require(healthy.backend.logURL == healthy.directory.appendingPathComponent("switch.log"),
                "backend log escaped its Switch fixture")
        healthy.start()
        let firstPID = healthy.backend.childPID
        healthy.start()
        require(healthy.backend.childPID == firstPID, "duplicate startup spawned another child")
        let becameHealthy = await waitUntil { healthy.backend.state == .running }
        require(becameHealthy, "authenticated healthy backend was not ready")
        healthy.start()
        require(healthy.backend.childPID == firstPID, "running backend spawned another child")
        await healthy.cleanup()
        require(healthy.backend.lastExit?.expected == true, "explicit stop should be recorded as expected")
        print("PASS authenticated health and duplicate starts")

        let gatewayReady = try Fixture(root: root, name: "go-start", mode: "go-starting")
        gatewayReady.start()
        let gatewayStarted = await waitUntil { gatewayReady.backend.state == .running }
        require(gatewayStarted, "Rust gateway readiness must not wait for Go hardware readiness")
        await gatewayReady.cleanup()
        print("PASS Rust gateway readiness is independent of Go hardware startup")

        let previousMode = UserDefaults.standard.string(forKey: "SwitchBackendMode")
        UserDefaults.standard.set("go-compatibility", forKey: "SwitchBackendMode")
        let fixedGateway = try Fixture(root: root, name: "fixed-rust-gateway", mode: "healthy")
        fixedGateway.start()
        let fixedGatewayStarted = await waitUntil { fixedGateway.backend.state == .running }
        require(fixedGatewayStarted, "legacy preference must not select an independent Go backend")
        await fixedGateway.cleanup()
        if let previousMode {
            UserDefaults.standard.set(previousMode, forKey: "SwitchBackendMode")
        } else {
            UserDefaults.standard.removeObject(forKey: "SwitchBackendMode")
        }
        print("PASS backend startup remains pinned to Rust gateway and /api/v2/runtime")

        let unexpected = try Fixture(root: root, name: "unexpected-exit", mode: "healthy")
        unexpected.start()
        let unexpectedStarted = await waitUntil { unexpected.backend.state == .running }
        require(unexpectedStarted, "unexpected-exit fixture did not start")
        _ = Darwin.kill(unexpected.backend.childPID, SIGKILL)
        let unexpectedFailed = await waitUntil { if case .failed = unexpected.backend.state { return true }; return false }
        require(unexpectedFailed, "unexpected running child exit should preserve a failure reason")
        require(unexpected.backend.lastExit?.status == SIGKILL && unexpected.backend.lastExit?.signaled == true,
                "unexpected signal was not recorded")
        require(unexpected.backend.lastExit?.expected == false, "unexpected child exit was labeled expected")
        await unexpected.cleanup()
        print("PASS unexpected child exit preserves signal/reason and retry state")

        for mode in ["bound", "unauthorized", "malformed", "false", "exit-after-request"] {
            let fixture = try Fixture(root: root, name: mode, mode: mode)
            fixture.start()
            try? await Task.sleep(nanoseconds: 200_000_000)
            require(fixture.backend.state != .running, "\(mode) incorrectly reported running")
            await fixture.cleanup()
        }
        print("PASS bound-only socket, unauthorized, malformed, ok=false, exit during health")

        // Advance only the injected monotonic clock. Actual HTTP attempts retain
        // their own short deadline, so this fixture never sleeps for 210 seconds.
        var silentUptime: TimeInterval = 0
        let silentObservation = BackendStartupObservation(
            uptime: { silentUptime },
            check: { timeout in
                let ready = await BackendReadiness.check(timeout: min(timeout, 0.05))
                if !ready { silentUptime += 30 }
                return ready
            },
            pause: { _ in await Task.yield() })
        let silent = try Fixture(root: root, name: "silent", mode: "silent",
                                 observation: silentObservation)
        silent.start()
        let silentPID = silent.backend.childPID
        let began = ProcessInfo.processInfo.systemUptime
        let failed = await waitUntil(seconds: 3) {
            if case .failed = silent.backend.state { return true }
            return false
        }
        let elapsed = ProcessInfo.processInfo.systemUptime - began
        require(failed && silentUptime >= 210 && elapsed < 3,
                "silent HTTP ignored the accelerated absolute observation deadline: \(elapsed)")
        require(Darwin.kill(silentPID, 0) == 0, "startup failure killed a live child")
        let threadsBeforeRepeatedTimeout = threadCount()
        let descriptorsBeforeRepeatedTimeout = fileDescriptorCount()
        for _ in 0..<12 { _ = await BackendReadiness.check(timeout: 0.05) }
        let threadsAfterRepeatedTimeout = threadCount()
        let descriptorsAfterRepeatedTimeout = fileDescriptorCount()
        require(threadsAfterRepeatedTimeout <= threadsBeforeRepeatedTimeout + 3, "timeout readers accumulated threads")
        require(descriptorsAfterRepeatedTimeout <= descriptorsBeforeRepeatedTimeout + 2, "timeout readers accumulated descriptors")
        print("RESOURCE 12 further timeouts: threads \(threadsBeforeRepeatedTimeout)→\(threadsAfterRepeatedTimeout), FDs \(descriptorsBeforeRepeatedTimeout)→\(descriptorsAfterRepeatedTimeout)")
        try "healthy".write(to: silent.directory.appendingPathComponent("mode"), atomically: true, encoding: .utf8)
        silent.start()
        require(silent.backend.childPID == silentPID, "failed readiness spawned a second live child")
        let retriedHealthy = await waitUntil { silent.backend.state == .running }
        require(retriedHealthy && silent.backend.childPID == silentPID, "explicit retry did not recover the existing child")
        let threadsBeforeCancellation = threadCount()
        let descriptorsBeforeCancellation = fileDescriptorCount()
        let accepted = silent.directory.appendingPathComponent("accepted")
        for _ in 0..<100 {
            try? FileManager.default.removeItem(at: accepted)
            try "silent".write(to: silent.directory.appendingPathComponent("mode"), atomically: true, encoding: .utf8)
            let pending = Task { await BackendReadiness.check(timeout: 1) }
            let received = await waitUntil { FileManager.default.fileExists(atPath: accepted.path) }
            require(received, "fixture did not receive cancellation probe")
            pending.cancel()
            let canceledResult = await pending.value
            require(!canceledResult, "canceled probe succeeded")
            try "healthy".write(to: silent.directory.appendingPathComponent("mode"), atomically: true, encoding: .utf8)
            let nextHealthy = await BackendReadiness.check(timeout: 1)
            require(nextHealthy, "canceled request interfered with the next healthy request")
        }
        let threadsAfterCancellation = threadCount()
        let descriptorsAfterCancellation = fileDescriptorCount()
        require(threadsAfterCancellation <= threadsBeforeCancellation + 3, "canceled readers accumulated threads")
        require(descriptorsAfterCancellation <= descriptorsBeforeCancellation + 2, "canceled readers accumulated descriptors")
        print("RESOURCE 100 cancellation/health pairs: threads \(threadsBeforeCancellation)→\(threadsAfterCancellation), FDs \(descriptorsBeforeCancellation)→\(descriptorsAfterCancellation)")
        await silent.cleanup()
        print("PASS silent listener deadline \(String(format: "%.3f", elapsed))s; explicit retry recovered original PID")

        let late = try Fixture(root: root, name: "late", mode: "late")
        late.start()
        try? await Task.sleep(nanoseconds: 150_000_000)
        late.backend.stop()
        require(late.backend.state == .stopping && late.backend.childPID > 0,
                "stop must cancel startup without inventing an exit receipt")
        try? await Task.sleep(nanoseconds: 500_000_000)
        require(late.backend.state == .stopping && late.backend.childPID > 0,
                "late successful health revived a stopping startup")
        await late.cleanup()
        print("PASS cancellation fences delayed successful health")

        let replacement = try Fixture(root: root, name: "replacement", mode: "exit")
        replacement.start()
        let retiredPID = replacement.backend.childPID
        // Keep the main actor occupied until the first child's termination is
        // queued, then replace it before either old callback can update state.
        usleep(250_000)
        try "healthy".write(to: replacement.directory.appendingPathComponent("mode"), atomically: true, encoding: .utf8)
        replacement.start()
        let replacementPID = replacement.backend.childPID
        require(retiredPID != replacementPID, "fixture failed to launch replacement")
        let replacementReady = await waitUntil { replacement.backend.state == .running }
        require(replacementReady && replacement.backend.childPID == replacementPID,
                "old startup/termination overwrote replacement state")
        await replacement.cleanup()
        print("PASS old startup/termination cannot overwrite replacement process")
        try await checkStopAndAppTermination(root: root)
    }
}
