import Darwin
import Foundation

private func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
}

private func ticket(_ admission: ReaderBLEOperationLedger.Admission) -> ReaderBLEOperationLedger.Ticket {
    guard case .dispatch(let ticket) = admission else { fatalError("expected dispatch ticket") }
    return ticket
}

private func reply(_ admission: ReaderBLEOperationLedger.Admission) -> ReaderBLEOperationLedger.Snapshot {
    guard case .reply(let reply) = admission else { fatalError("unexpected physical dispatch") }
    return reply
}

private func ledgerTests() {
    typealias Ledger = ReaderBLEOperationLedger
    let ledger = Ledger(capacity: 3, bridgeID: "epoch-a")
    let fp = Ledger.Fingerprint(action: "apdu", reader: "BLE:fixture", hex: "00840000")
    let first = ticket(ledger.admit(operationID: "job-a", bridgeID: "epoch-a", fingerprint: fp))
    expect(!ledger.status(operationID: "job-a", bridgeID: "epoch-a").terminal, "queued is not terminal")
    let duplicate = reply(ledger.admit(operationID: "job-a", bridgeID: "epoch-a", fingerprint: fp))
    expect(duplicate.result.errorCode == "ble_operation_pending", "duplicate queues no new APDU")
    let conflict = reply(ledger.admit(operationID: "job-a", bridgeID: "epoch-a",
                                      fingerprint: .init(action: "reset", reader: fp.reader, hex: nil)))
    expect(!conflict.terminal && conflict.result.errorCode == "ble_operation_conflict", "conflict is not completion")
    let lock = NSLock()
    var winners = 0
    DispatchQueue.concurrentPerform(iterations: 64) { _ in
        if ledger.begin(first) { lock.lock(); winners += 1; lock.unlock() }
    }
    expect(winners == 1, "only one main closure can claim a queued operation")
    let busy = reply(ledger.admit(operationID: "job-b", bridgeID: "epoch-a", fingerprint: fp))
    expect(busy.terminal && busy.result.errorCode == "ble_busy", "one pending operation maximum")
    expect(ledger.status(operationID: "job-b", bridgeID: "epoch-a") == busy, "rejection survives dropped reply")
    expect(!ledger.status(operationID: "job-a", bridgeID: "epoch-b").terminal, "wrong epoch is no proof")
    expect(!ledger.status(operationID: "unknown", bridgeID: "epoch-a").terminal, "unknown is no proof")
    expect(!reply(ledger.admit(operationID: "../invalid", bridgeID: "epoch-a", fingerprint: fp)).terminal,
           "invalid identity cannot prove completion")
    let terminal = Ledger.Result(ok: true, hex: "9000")
    expect(ledger.finish(first, result: terminal), "physical callback publishes terminal")
    expect(!ledger.finish(first, result: .init(ok: false, errorCode: "late")), "late callback cannot overwrite")
    expect(ledger.status(operationID: "job-a", bridgeID: "epoch-a").result == terminal, "original result retained")
    expect(reply(ledger.admit(operationID: "job-a", bridgeID: "epoch-a", fingerprint: fp)).terminal,
           "duplicate completed operation is not replayed")

    let bounded = Ledger(capacity: 2, bridgeID: "epoch-b")
    let running = ticket(bounded.admit(operationID: "held", bridgeID: "epoch-b", fingerprint: fp))
    expect(bounded.begin(running), "start held job")
    for index in 0..<200 {
        let rejected = reply(bounded.admit(operationID: "reject-\(index)", bridgeID: "epoch-b", fingerprint: fp))
        expect(rejected.terminal, "busy requests have non-dispatch receipts")
    }
    expect(bounded.status(operationID: "held", bridgeID: "epoch-b").result.errorCode == "ble_operation_pending",
           "bounded eviction never removes a live physical operation")
    expect(bounded.status(operationID: "reject-0", bridgeID: "epoch-b").result.errorCode == "ble_operation_unknown",
           "evicted terminal receipt is not proof of completion")
    expect(bounded.finish(running, result: terminal), "retained running operation can finish")
    let validation = Ledger.Result(ok: false, errorCode: "ble_invalid_apdu")
    let invalid = reply(bounded.admit(operationID: "invalid-payload", bridgeID: "epoch-b",
                                      fingerprint: fp, rejection: validation))
    expect(invalid.terminal && invalid.result == validation, "validated pre-dispatch rejection is terminal")

    let reused = Ledger(capacity: 1, bridgeID: "epoch-c")
    let old = ticket(reused.admit(operationID: "same-id", bridgeID: "epoch-c", fingerprint: fp))
    expect(reused.begin(old), "start old incarnation")
    expect(reused.finish(old, result: terminal), "finish old incarnation")
    _ = reply(reused.admit(operationID: "evict", bridgeID: "epoch-c", fingerprint: fp, rejection: validation))
    let new = ticket(reused.admit(operationID: "same-id", bridgeID: "epoch-c", fingerprint: fp))
    expect(!reused.begin(old) && !reused.finish(old, result: terminal), "stale callbacks cannot touch a reused ID")
    expect(reused.begin(new), "new incarnation is independently guarded")
    let firstInstance = Ledger(capacity: 1, bridgeID: "reused-epoch")
    let staleTicket = ticket(firstInstance.admit(operationID: "same-id", bridgeID: "reused-epoch", fingerprint: fp))
    let replacementInstance = Ledger(capacity: 1, bridgeID: "reused-epoch")
    let replacementTicket = ticket(replacementInstance.admit(operationID: "same-id", bridgeID: "reused-epoch", fingerprint: fp))
    expect(!replacementInstance.begin(staleTicket), "old bridge ticket cannot claim a replacement bridge job")
    expect(replacementInstance.begin(replacementTicket), "replacement bridge accepts its own ticket")
    expect(!replacementInstance.finish(staleTicket, result: terminal), "old callback cannot finish replacement bridge job")
    expect(!replacementInstance.status(operationID: "same-id", bridgeID: "reused-epoch").terminal,
           "stale cross-bridge callback leaves replacement job pending")
    expect(replacementInstance.finish(replacementTicket, result: terminal), "replacement callback completes its own job")
    let connections=Ledger(capacity:8,bridgeID:"connection-bridge")
    expect(connections.registerConnection(reader:"BLE:old",connectionID:"epoch-old",operationID:"close-old"),"register exact old connection")
    let apdu=ticket(connections.admit(operationID:"card-old",bridgeID:"connection-bridge",
        fingerprint:.init(action:"apdu",reader:"BLE:old",hex:"00",connectionID:"epoch-old")))
    expect(connections.begin(apdu),"begin exact card exchange")
    connections.confirmDisconnected(reader:"BLE:old",connectionID:"epoch-wrong",operationID:"close-old")
    expect(!connections.status(operationID:"close-old",bridgeID:"connection-bridge").terminal,"stale physical callback is no proof")
    connections.confirmDisconnected(reader:"BLE:old",connectionID:"epoch-old",operationID:"close-old")
    expect(!connections.status(operationID:"close-old",bridgeID:"connection-bridge").terminal,"disconnect cannot bypass running APDU completion")
    expect(connections.finish(apdu,result:.init(ok:false,errorCode:"bluetooth_disconnected")),"actual card completion")
    let closed=connections.status(operationID:"close-old",bridgeID:"connection-bridge")
    expect(closed.terminal && closed.closed == true && closed.connectionID == "epoch-old" && closed.reader == "BLE:old","exact terminal close receipt")
    expect(connections.registerConnection(reader:"BLE:old",connectionID:"epoch-new",operationID:"close-new"),"replacement connection")
    connections.confirmDisconnected(reader:"BLE:old",connectionID:"epoch-old",operationID:"close-old")
    expect(!connections.status(operationID:"close-new",bridgeID:"connection-bridge").terminal,"late old proof cannot close replacement")
    expect(!connections.registerConnection(reader:"BLE:old",connectionID:"epoch-new",operationID:"close-old"),"close operation identity cannot be rebound")
    print("BLE ledger: duplicate/CAS, conflict, pending fence, epoch/unknown, rejection receipt, bounded eviction and stale callback passed")
}

private func wire(_ path: String, _ request: [String: Any], dropReply: Bool = false) -> [String: Any] {
    let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    expect(fd >= 0, "create fixture socket")
    var noSignal: Int32 = 1
    _ = Darwin.setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
    defer { Darwin.close(fd) }
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    let bytes = Array(path.utf8)
    withUnsafeMutableBytes(of: &address.sun_path) { storage in
        expect(bytes.count < storage.count, "socket path length")
        storage.copyBytes(from: bytes)
        storage[bytes.count] = 0
    }
    let connected = withUnsafePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    expect(connected == 0, "connect fixture socket")
    var timeout = timeval(tv_sec: 3, tv_usec: 0)
    _ = Darwin.setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    var data = try! JSONSerialization.data(withJSONObject: request)
    data.append(10)
    data.withUnsafeBytes { raw in
        expect(Darwin.write(fd, raw.baseAddress, raw.count) == raw.count, "send fixture request")
    }
    if dropReply { return [:] }
    var result = Data()
    var buffer = [UInt8](repeating: 0, count: 4096)
    while !result.contains(10) {
        let count = Darwin.read(fd, &buffer, buffer.count)
        expect(count > 0, "fixture response must arrive without main-queue deadlock")
        result.append(contentsOf: buffer.prefix(count))
    }
    return try! JSONSerialization.jsonObject(with: result) as! [String: Any]
}

private func bridgeTests(_ path: String) {
    let auth = LocalBackendAuthentication.appToken
    let state = wire(path, ["token": auth, "action": "status"])
    expect(state["operation_tracking"] as? Bool == true, "advertise operation proof capability")
    let epoch = state["bridge_id"] as! String
    let reader = state["reader"] as! String
    func request(_ id: String, action: String = "apdu", hex: String = "00840000") -> [String: Any] {
        ["token": auth, "action": action, "reader": reader, "hex": hex,
         "operation_id": id, "bridge_id": epoch]
    }
    func status(_ id: String, bridgeID: String? = nil) -> [String: Any] {
        wire(path, ["token": auth, "action": "operation-status", "operation_id": id,
                    "bridge_id": bridgeID ?? epoch])
    }
    func awaitTerminal(_ id: String) -> [String: Any] {
        for _ in 0..<200 {
            let result = status(id)
            if result["terminal"] as? Bool == true { return result }
            usleep(5_000)
        }
        fatalError("fixture operation did not finish")
    }
    let blocked = DispatchSemaphore(value: 0), unblock = DispatchSemaphore(value: 0)
    DispatchQueue.main.async { blocked.signal(); unblock.wait() }
    expect(blocked.wait(timeout: .now() + 1) == .success, "block fixture main queue")
    let queued = wire(path, request("queued"))
    expect(queued["terminal"] as? Bool == false && queued["errorCode"] as? String == "ble_operation_pending",
           "socket timeout cannot finish queued card work")
    let startedAt = Date()
    expect(status("queued")["terminal"] as? Bool == false, "query queued operation off main queue")
    expect(Date().timeIntervalSince(startedAt) < 0.5, "operation-status does not await main queue")
    expect(BluetoothReaderManager.shared.cardCalls == 0, "queued request has not touched the card")
    let duplicate = wire(path, request("queued"))
    expect(duplicate["errorCode"] as? String == "ble_operation_pending", "same-ID submit does not queue again")
    let conflict = wire(path, request("queued", hex: "00A40000"))
    expect(conflict["terminal"] as? Bool == false && conflict["errorCode"] as? String == "ble_operation_conflict",
           "different payload cannot finish original job")
    expect(status("queued", bridgeID: "old-epoch")["terminal"] as? Bool == false, "old epoch cannot release")
    expect(status("unknown")["terminal"] as? Bool == false, "unknown job cannot release")
    let busy = wire(path, request("other"))
    expect(busy["terminal"] as? Bool == true && busy["errorCode"] as? String == "ble_busy", "second job rejected before card")
    unblock.signal()
    let finished = awaitTerminal("queued")
    expect(finished["ok"] as? Bool == true && finished["hex"] as? String == "9000", "actual callback result retained")
    expect(finished["operation_id"] as? String == "queued" && finished["bridge_id"] as? String == epoch, "exact terminal binding")
    expect(BluetoothReaderManager.shared.cardCalls == 1, "only one APDU dispatched")
    _ = wire(path, request("queued"))
    expect(BluetoothReaderManager.shared.cardCalls == 1, "terminal replay only returns receipt")
    _ = wire(path, request("lost-response"), dropReply: true)
    let recovered = awaitTerminal("lost-response")
    expect(recovered["hex"] as? String == "9000" && BluetoothReaderManager.shared.cardCalls == 2,
           "lost response recovers through status without APDU replay")
    let invalid = wire(path, request("invalid", hex: "not-hex"))
    expect(invalid["terminal"] as? Bool == true && invalid["errorCode"] as? String == "ble_invalid_apdu", "invalid APDU receipt")
    expect(status("invalid")["terminal"] as? Bool == true, "invalid receipt retained without dispatch")
    BluetoothReaderManager.shared.holdNextCardCompletion()
    let legacy = wire(path, ["token": auth, "action": "apdu", "reader": reader, "hex": "00840000"])
    expect(BluetoothReaderManager.shared.cardCompletionHeld.wait(timeout: .now() + 2) == .success,
           "legacy physical callback is held before timeout assertions")
    expect(legacy["operation_id"] == nil && legacy["terminal"] == nil, "legacy response shape retained")
    expect(legacy["errorCode"] as? String == "ble_timeout", "legacy wait timeout remains compatible")
    expect(wire(path, request("legacy-fence"))["errorCode"] as? String == "ble_busy", "legacy pending work shares physical fence")
    expect(BluetoothReaderManager.shared.releaseHeldCardCompletion().wait(timeout: .now() + 2) == .success,
           "legacy physical callback and ledger completion finished")
    BluetoothReaderManager.shared.holdNextCardCompletion()
    let reset = wire(path, request("reset", action: "reset"))
    expect(BluetoothReaderManager.shared.cardCompletionHeld.wait(timeout: .now() + 2) == .success,
           "reset physical callback is held before timeout assertions")
    expect(reset["terminal"] as? Bool == false, "reset timeout also retains ownership")
    expect(BluetoothReaderManager.shared.releaseHeldCardCompletion().wait(timeout: .now() + 2) == .success,
           "reset physical callback and ledger completion finished")
    expect(awaitTerminal("reset")["ok"] as? Bool == true, "reset completion is tracked")
    let connection=state["connection_id"] as! String
    let closeID=state["close_operation_id"] as! String
    expect(status(closeID)["terminal"] as? Bool == false,"live connection has no close receipt")
    var wrong=request("wrong-connection"); wrong["connection_id"]="superseded-connection"
    _=wire(path,wrong)
    let rejected=awaitTerminal("wrong-connection")
    expect(rejected["ok"] as? Bool == false && rejected["errorCode"] as? String == "ble_reader_changed","stale connection cannot dispatch an APDU")
    _=wire(path,["token":auth,"action":"disconnect"])
    expect(status(closeID)["terminal"] as? Bool == false,"logical disconnect must not complete close operation")
    DispatchQueue.main.async {
        let manager=BluetoothReaderManager.shared
        manager.physicalConnectionClosed?(manager.identity)
    }
    let closed=awaitTerminal(closeID)
    expect(closed["closed"] as? Bool == true && closed["connection_id"] as? String == connection
        && closed["reader"] as? String == reader,"exact close proof survives disconnected UI")
    print("BLE Unix bridge: capability, queued/main-blocked query, duplicate/conflict, timeout, dropped reply, legacy fence and reset proof passed")
}

private func socketExists(_ path: String) -> Bool {
    var info = stat()
    return Darwin.lstat(path, &info) == 0 &&
        (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK)
}

private struct SocketIdentity: Equatable {
    let device: UInt64
    let inode: UInt64
}

private func socketIdentity(_ path: String) -> SocketIdentity? {
    var info = stat()
    guard Darwin.lstat(path, &info) == 0,
          (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK) else { return nil }
    return SocketIdentity(device: UInt64(info.st_dev), inode: UInt64(info.st_ino))
}

private func socketMode(_ path: String) -> mode_t? {
    var info = stat()
    guard Darwin.lstat(path, &info) == 0,
          (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK) else { return nil }
    return info.st_mode & 0o777
}

private func waitForDescriptorClosed(_ descriptor: Int32, timeout: TimeInterval = 2) {
    let deadline = ProcessInfo.processInfo.systemUptime + timeout
    repeat {
        errno = 0
        if Darwin.fcntl(descriptor, F_GETFD, 0) == -1 && errno == EBADF { return }
        usleep(5_000)
    } while ProcessInfo.processInfo.systemUptime < deadline
    fatalError("BLE bridge did not close its unpublished listener")
}

private func waitForSocketRemoved(_ path: String, timeout: TimeInterval = 2) {
    let deadline = ProcessInfo.processInfo.systemUptime + timeout
    repeat {
        if !socketExists(path) { return }
        usleep(5_000)
    } while ProcessInfo.processInfo.systemUptime < deadline
    fatalError("BLE bridge did not remove its private temporary socket")
}

private func waitForBridge(_ path: String, timeout: TimeInterval = 2) {
    let deadline = ProcessInfo.processInfo.systemUptime + timeout
    repeat {
        if socketExists(path) {
            let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
            if descriptor >= 0 {
                var address = sockaddr_un()
                address.sun_family = sa_family_t(AF_UNIX)
                withUnsafeMutableBytes(of: &address.sun_path) { storage in
                    let bytes = Array(path.utf8)
                    storage.copyBytes(from: bytes)
                    storage[bytes.count] = 0
                }
                let connected = withUnsafePointer(to: &address) { pointer in
                    pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                    }
                }
                Darwin.close(descriptor)
                if connected == 0 { return }
            }
        }
        usleep(5_000)
    } while ProcessInfo.processInfo.systemUptime < deadline
    fatalError("BLE bridge did not become reachable")
}

private func bindFixtureSocket(_ path: String, listen: Bool) -> Int32 {
    let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    expect(descriptor >= 0, "create lifecycle fixture socket")
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &address.sun_path) { storage in
        let bytes = Array(path.utf8)
        storage.copyBytes(from: bytes)
        storage[bytes.count] = 0
    }
    let bound = withUnsafePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    expect(bound == 0, "bind lifecycle fixture socket")
    expect(Darwin.chmod(path, 0o600) == 0, "make lifecycle fixture socket private")
    if listen { expect(Darwin.listen(descriptor, 1) == 0, "listen lifecycle fixture socket") }
    return descriptor
}

private func startupStaleSocketTests(_ directory: String) {
    let livePath = directory + "/live.sock"
    let live = bindFixtureSocket(livePath, listen: true)
    let liveIdentity = socketIdentity(livePath)
    let liveAttemptFinished = DispatchSemaphore(value: 0)
    let contender = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01,
                                    staleProbeTimeout: 0.05,
                                    afterStartupAttempt: { liveAttemptFinished.signal() })
    let startBegan = ProcessInfo.processInfo.systemUptime
    contender.start(socketPath: livePath)
    expect(ProcessInfo.processInfo.systemUptime - startBegan < 0.02,
           "bridge start returns without probing a live socket on the caller")
    expect(liveAttemptFinished.wait(timeout: .now() + 2) == .success, "live ownership probe completed")
    expect(socketIdentity(livePath) == liveIdentity,
           "live listener survives a competing bridge start")
    contender.stop()
    Darwin.close(live)
    expect(Darwin.unlink(livePath) == 0, "remove live fixture socket")

    let fullPath = directory + "/full.sock"
    let full = bindFixtureSocket(fullPath, listen: true)
    let fullIdentity = socketIdentity(fullPath)
    var queuedClients: [Int32] = []
    for _ in 0..<32 {
        let client = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        expect(client >= 0, "create accept-queue fixture client")
        let flags = Darwin.fcntl(client, F_GETFL, 0)
        expect(flags >= 0 && Darwin.fcntl(client, F_SETFL, flags | O_NONBLOCK) == 0,
               "make accept-queue fixture client nonblocking")
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &address.sun_path) { storage in
            let bytes = Array(fullPath.utf8)
            storage.copyBytes(from: bytes)
            storage[bytes.count] = 0
        }
        _ = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(client, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        queuedClients.append(client)
    }
    let fullAttemptFinished = DispatchSemaphore(value: 0)
    let fullContender = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01,
                                        staleProbeTimeout: 0.05,
                                        afterStartupAttempt: { fullAttemptFinished.signal() })
    let fullStartBegan = ProcessInfo.processInfo.systemUptime
    fullContender.start(socketPath: fullPath)
    expect(ProcessInfo.processInfo.systemUptime - fullStartBegan < 0.02,
           "saturated accept queue never blocks bridge start caller")
    expect(fullAttemptFinished.wait(timeout: .now() + 2) == .success, "saturated ownership probe completed")
    expect(socketIdentity(fullPath) == fullIdentity,
           "timeout or uncertain probe preserves the saturated listener")
    fullContender.stop()
    queuedClients.forEach { Darwin.close($0) }
    Darwin.close(full)
    expect(Darwin.unlink(fullPath) == 0, "remove saturated fixture socket")

    let stalePath = directory + "/stale.sock"
    let stale = bindFixtureSocket(stalePath, listen: false)
    Darwin.close(stale)
    let staleIdentity = socketIdentity(stalePath)
    let bridge = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01,
                                 staleProbeTimeout: 0.05)
    bridge.start(socketPath: stalePath)
    waitForBridge(stalePath)
    expect(socketIdentity(stalePath) != staleIdentity,
           "confirmed stale socket is replaced by the bridge")
    bridge.stop()

    let swappedPath = directory + "/swapped.sock"
    let first = bindFixtureSocket(swappedPath, listen: false)
    Darwin.close(first)
    var successor: Int32 = -1
    var successorIdentity: SocketIdentity?
    let swapped = DispatchSemaphore(value: 0)
    let guarded = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01,
                                  staleProbeTimeout: 0.05,
                                  beforeStaleUnlink: {
        expect(Darwin.unlink(swappedPath) == 0, "replace stale pathname in TOCTOU fixture")
        successor = bindFixtureSocket(swappedPath, listen: true)
        successorIdentity = socketIdentity(swappedPath)
        swapped.signal()
    })
    guarded.start(socketPath: swappedPath)
    expect(swapped.wait(timeout: .now() + 2) == .success,
           "stale probe reached the guarded unlink point")
    expect(successor >= 0 && socketIdentity(swappedPath) == successorIdentity,
           "stale cleanup preserves a successor socket with a different inode")
    guarded.stop()
    expect(socketIdentity(swappedPath) == successorIdentity,
           "stopped contender does not unlink successor socket")
    Darwin.close(successor)
    expect(Darwin.unlink(swappedPath) == 0, "remove successor fixture socket")

    let publishRacePath = directory + "/publish-race.sock"
    var unpublishedDescriptor: Int32 = -1
    var temporaryPath: String?
    var publishSuccessor: Int32 = -1
    var publishSuccessorIdentity: SocketIdentity?
    let replacedAfterBind = DispatchSemaphore(value: 0)
    let publishContender = ReaderBLEBridge(
        standardRequestTimeout: 1, cardOperationTimeout: 0.01,
        staleProbeTimeout: 0.05,
        afterBindBeforePublish: { candidate, descriptor in
            temporaryPath = candidate
            unpublishedDescriptor = descriptor
            publishSuccessor = bindFixtureSocket(publishRacePath, listen: true)
            expect(Darwin.chmod(publishRacePath, 0o640) == 0,
                   "set successor mode before atomic publish race")
            publishSuccessorIdentity = socketIdentity(publishRacePath)
            replacedAfterBind.signal()
        })
    publishContender.start(socketPath: publishRacePath)
    expect(replacedAfterBind.wait(timeout: .now() + 2) == .success,
           "post-bind replacement hook ran before public socket publish")
    waitForDescriptorClosed(unpublishedDescriptor)
    expect(socketIdentity(publishRacePath) == publishSuccessorIdentity,
           "exclusive publish preserves a successor socket")
    expect(socketMode(publishRacePath) == 0o640,
           "failed publish does not chmod the successor socket")
    if let temporaryPath {
        waitForSocketRemoved(temporaryPath)
    } else {
        fatalError("post-bind hook did not expose its private pathname")
    }
    publishContender.stop()
    expect(socketIdentity(publishRacePath) == publishSuccessorIdentity,
           "stopped unpublished bridge does not delete the successor socket")
    expect(socketMode(publishRacePath) == 0o640,
           "stopped unpublished bridge does not mutate successor permissions")
    Darwin.close(publishSuccessor)
    expect(Darwin.unlink(publishRacePath) == 0, "remove publish-race successor fixture")
    print("BLE Unix bridge: async bounded stale probe, exclusive publish and inode-safe replacement passed")
}

private func bridgeLifecycleTests(_ bridge: ReaderBLEBridge, _ path: String) {
    bridge.stop()
    expect(!socketExists(path), "normal bridge stop removes its own socket")
    bridge.start(socketPath: path)
    waitForBridge(path)
    let restartedState = wire(path, ["token": LocalBackendAuthentication.appToken, "action": "status"])
    let priorEpoch = restartedState["bridge_id"] as! String
    expect(restartedState["ok"] as? Bool == true,
           "bridge serves authenticated requests after restart")

    // Simulate another instance replacing the pathname while the old listener
    // is still open. The old bridge may close its fd but must not unlink the
    // other instance's socket.
    expect(Darwin.unlink(path) == 0, "remove only test bridge pathname")
    let foreign = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    expect(foreign >= 0, "create foreign listener")
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &address.sun_path) { storage in
        let bytes = Array(path.utf8)
        storage.copyBytes(from: bytes)
        storage[bytes.count] = 0
    }
    let bound = withUnsafePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.bind(foreign, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    expect(bound == 0 && Darwin.listen(foreign, 1) == 0, "foreign listener owns replacement socket")
    bridge.stop()
    expect(socketExists(path), "bridge stop preserves another instance's socket")
    Darwin.close(foreign)
    expect(Darwin.unlink(path) == 0, "remove test foreign socket")
    expect(FileManager.default.createFile(atPath: path, contents: Data("sentinel".utf8)),
           "create non-socket path guard fixture")
    let checkedNonSocket = DispatchSemaphore(value: 0)
    let fileGuard = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01,
                                   afterStartupAttempt: { checkedNonSocket.signal() })
    fileGuard.start(socketPath: path)
    expect(checkedNonSocket.wait(timeout: .now() + 2) == .success,
           "non-socket startup attempt completed before fixture removal")
    expect((try? Data(contentsOf: URL(fileURLWithPath: path))) == Data("sentinel".utf8),
           "completed bridge start preserves an existing non-socket file")
    fileGuard.stop()
    try! FileManager.default.removeItem(atPath: path)
    let replacement = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01)
    replacement.start(socketPath: path)
    waitForBridge(path)
    let replacementState = wire(path, ["token": LocalBackendAuthentication.appToken, "action": "status"])
    let replacementEpoch = replacementState["bridge_id"] as! String
    expect(replacementEpoch != priorEpoch, "new bridge instance has a separate proof epoch")
    let oldProof = wire(path, ["token": LocalBackendAuthentication.appToken, "action": "operation-status",
                               "operation_id": "queued", "bridge_id": priorEpoch])
    expect(oldProof["terminal"] as? Bool == false && oldProof["errorCode"] as? String == "ble_bridge_changed",
           "replacement bridge rejects an earlier epoch's receipt")
    replacement.stop()
    print("BLE Unix bridge: own-socket cleanup, restart and foreign/non-socket preservation passed")
}

private final class AcceptRetirementProbe: @unchecked Sendable {
    let entered = DispatchSemaphore(value: 0)
    let release = DispatchSemaphore(value: 0)
    let retired = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var current: Int32 = -1
    private var results: [(Int32, Int32)] = []
    func hold(_ descriptor: Int32) {
        lock.lock(); current = descriptor; lock.unlock()
        entered.signal()
        expect(release.wait(timeout: .now() + 10) == .success, "paused accept worker was released")
    }
    func finished(_ descriptor: Int32, _ result: Int32) {
        lock.lock(); results.append((descriptor, result)); lock.unlock()
        retired.signal()
    }
    var descriptor: Int32 { lock.lock(); defer { lock.unlock() }; return current }
    var closes: [(Int32, Int32)] { lock.lock(); defer { lock.unlock() }; return results }
}

private func listenerRetirementTests(_ directory: String) {
    // Pause before weak-self acquisition: both a living bridge and a deallocated
    // bridge must leave fd closure exclusively to the queued accept worker.
    for keepBridge in [true, false] {
        let path = directory + (keepBridge ? "/owned.sock" : "/gone.sock")
        let probe = AcceptRetirementProbe()
        let startup = DispatchSemaphore(value: 0)
        var old: ReaderBLEBridge? = ReaderBLEBridge(
            afterStartupAttempt: { startup.signal() },
            beforeAcceptWorker: probe.hold, afterAcceptRetired: probe.finished)
        weak var weakOld = old
        old!.start(socketPath: path)
        expect(startup.wait(timeout: .now() + 2) == .success, "paused listener startup completed")
        expect(probe.entered.wait(timeout: .now() + 2) == .success, "accept worker reached its barrier")
        let oldFD = probe.descriptor
        let listenerFlags = Darwin.fcntl(oldFD, F_GETFL, 0)
        expect(listenerFlags >= 0 && listenerFlags & O_NONBLOCK != 0, "listener must be nonblocking")
        old!.stop()
        expect(!socketExists(path), "stop unpublishes the owned socket")
        expect(Darwin.fcntl(oldFD, F_GETFD, 0) >= 0,
               "stop closed a descriptor still owned by a queued accept worker")
        if !keepBridge {
            old = nil
            let deadline = ProcessInfo.processInfo.systemUptime + 2
            while weakOld != nil && ProcessInfo.processInfo.systemUptime < deadline { usleep(1_000) }
            expect(weakOld == nil, "paused worker did not preserve weak-self lifetime semantics")
        }

        let successorStarted = DispatchSemaphore(value: 0)
        let successorRetired = AcceptRetirementProbe()
        var successorFD: Int32 = -1
        let successor = ReaderBLEBridge(
            afterBindBeforePublish: { _, descriptor in successorFD = descriptor },
            afterStartupAttempt: { successorStarted.signal() },
            afterAcceptRetired: successorRetired.finished)
        successor.start(socketPath: path)
        expect(successorStarted.wait(timeout: .now() + 2) == .success, "successor startup completed")
        waitForBridge(path)
        expect(successorFD != oldFD, "successor reused a descriptor still owned by the paused worker")
        let before = wire(path, ["token": LocalBackendAuthentication.appToken, "action": "status"])
        expect(before["ok"] as? Bool == true, "successor serves while predecessor is paused")
        probe.release.signal()
        expect(probe.retired.wait(timeout: .now() + 2) == .success, "predecessor worker actually retired")
        expect(probe.closes.count == 1 && probe.closes[0].0 == oldFD && probe.closes[0].1 == 0,
               "predecessor did not close its own descriptor exactly once")
        let after = wire(path, ["token": LocalBackendAuthentication.appToken, "action": "status"])
        expect(after["ok"] as? Bool == true && after["bridge_id"] as? String == before["bridge_id"] as? String,
               "retired worker consumed or closed a successor connection")
        old?.stop()
        expect(wire(path, ["token": LocalBackendAuthentication.appToken, "action": "status"])["ok"] as? Bool == true,
               "stopped predecessor damaged the successor listener")
        successor.stop()
        expect(successorRetired.retired.wait(timeout: .now() + 2) == .success, "idle poll retired within its budget")
        expect(successorRetired.closes.count == 1 && successorRetired.closes[0].1 == 0,
               "successor listener was not closed exactly once")
    }

    // Inject a single EIO poll result only in this test build. The real error
    // branch and unique fd owner must retire and clear state before a restart.
    let faultPath = directory + "/fault.sock"
    let fault = AcceptRetirementProbe()
    let startup = DispatchSemaphore(value: 0)
    let recoverable = ReaderBLEBridge(afterStartupAttempt: { startup.signal() },
        beforeAcceptWorker: fault.hold, afterAcceptRetired: fault.finished)
    recoverable.start(socketPath: faultPath)
    expect(startup.wait(timeout: .now() + 2) == .success && fault.entered.wait(timeout: .now() + 2) == .success,
           "fault listener reached its worker barrier")
    recoverable.failNextAcceptPollForTesting()
    fault.release.signal()
    expect(fault.retired.wait(timeout: .now() + 2) == .success, "injected poll failure did not retire its listener")
    expect(!socketExists(faultPath) && fault.closes.count == 1 && fault.closes[0].1 == 0,
           "injected-error retirement left published state or leaked its descriptor")
    recoverable.start(socketPath: faultPath)
    expect(startup.wait(timeout: .now() + 2) == .success && fault.entered.wait(timeout: .now() + 2) == .success,
           "injected-error retirement did not permit a new listener")
    fault.release.signal()
    waitForBridge(faultPath)
    expect(wire(faultPath, ["token": LocalBackendAuthentication.appToken, "action": "status"])["ok"] as? Bool == true,
           "listener failed to recover after injected poll-error retirement")
    recoverable.stop()
    expect(fault.retired.wait(timeout: .now() + 2) == .success && fault.closes.count == 2
           && fault.closes.allSatisfy { $0.1 == 0 }, "restarted listener retirement leaked or double-closed")

    let rapidPath = directory + "/rapid.sock"
    let rapid = AcceptRetirementProbe()
    let bridge = ReaderBLEBridge(afterAcceptRetired: rapid.finished)
    for _ in 0..<12 {
        bridge.start(socketPath: rapidPath)
        waitForBridge(rapidPath)
        expect(wire(rapidPath, ["token": LocalBackendAuthentication.appToken, "action": "status"])["ok"] as? Bool == true,
               "rapid restart lost authenticated status")
        bridge.stop() // Deliberately do not join before starting the successor.
    }
    for _ in 0..<12 {
        expect(rapid.retired.wait(timeout: .now() + 2) == .success, "rapid restart leaked a retired worker")
    }
    expect(rapid.closes.count == 12 && rapid.closes.allSatisfy { $0.1 == 0 }, "rapid restart did not close each listener once")
    print("BLE Unix bridge: queued/weak-self/idle/injected-error retirement, successor isolation and rapid restart passed")
}

private func temporarySocketPathBoundaryTests() {
    let build = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SWITCH_BUILD_DIR"]!)
    let root = build.appendingPathComponent("tmp/bind-" + String(UUID().uuidString.prefix(8)))
    try! FileManager.default.createDirectory(at: root, withIntermediateDirectories: true,
                                             attributes: [.posixPermissions: 0o700])
    defer { try! FileManager.default.removeItem(at: root) }
    let maximum = MemoryLayout<sockaddr_un>.size - 4
    // Full UUID, first shortened UUID, and the minimum permitted 48 random bits.
    for directoryBytes in [maximum - 36, maximum - 35, maximum - 16] {
        let padding = directoryBytes - root.path.utf8.count - 1
        expect(padding > 0, "fixture root must leave room for exact socket path boundaries")
        let directory = root.appendingPathComponent(String(repeating: "p", count: padding))
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                                                 attributes: [.posixPermissions: 0o700])
        expect(directory.path.utf8.count == directoryBytes, "fixture directory byte length changed")
        let publicPath = directory.appendingPathComponent("s").path
        let bound = DispatchSemaphore(value: 0)
        var captured: String?
        let bridge = ReaderBLEBridge(afterBindBeforePublish: { candidate, _ in
            captured = candidate
            bound.signal()
        })
        bridge.start(socketPath: publicPath)
        expect(bound.wait(timeout: .now() + 2) == .success,
               "valid long directory could not bind its private temporary socket")
        waitForBridge(publicPath)
        let candidate = captured!
        let name = (candidate as NSString).lastPathComponent
        expect(candidate.utf8.count == maximum - 1, "temporary socket violated strict pathname boundary")
        expect(name.hasPrefix(".b") && name.utf8.count - 2 == min(32, maximum - directoryBytes - 4),
               "temporary name lost its expected random suffix budget")
        expect(name.utf8.count - 2 >= 12, "temporary socket reduced collision resistance below 48 bits")
        bridge.stop()
        waitForSocketRemoved(publicPath)
        expect(!socketExists(candidate), "private temporary socket survived publication/stop")
    }
    print("BLE Unix bridge: exact full/shortened/minimum-random temporary path boundaries passed")
}

@main
private enum ReaderBLEOperationProofTests {
    static func main() {
        setbuf(stdout, nil)
        ledgerTests()
        temporarySocketPathBoundaryTests()
        let bridge = ReaderBLEBridge(standardRequestTimeout: 1, cardOperationTimeout: 0.01)
        let path = CommandLine.arguments[1]
        bridge.start(socketPath: path)
        waitForBridge(path)
        DispatchQueue.global().async {
            bridgeTests(path)
            bridgeLifecycleTests(bridge, path)
            startupStaleSocketTests((path as NSString).deletingLastPathComponent)
            listenerRetirementTests((path as NSString).deletingLastPathComponent)
            print("Reader BLE operation proof tests passed (no hardware)")
            exit(0)
        }
        dispatchMain()
    }
}
