import Darwin
import Foundation

/// Private, authenticated Unix-socket bridge from the backend's card services
/// to the app-owned CoreBluetooth central. The ledger serializes card jobs;
/// independent bounded socket workers can query completion off the main queue.
final class ReaderBLEBridge {
    static let shared = ReaderBLEBridge()
    private var listener: Int32 = -1
    private let lifecycleLock = NSLock()
    private let startupQueue = DispatchQueue(label: "com.proletarius.switch.ble-bridge-startup",
                                             qos: .userInitiated)
    private var listenerGeneration: UInt64 = 0
    private var pendingStartGeneration: UInt64?
    private var boundSocketPath: String?
    private var boundSocketIdentity: (device: UInt64, inode: UInt64)?
    private let maxRequestBytes = 160_000
    private let standardRequestTimeout: TimeInterval
    private let cardOperationTimeout: TimeInterval
    private let operations = ReaderBLEOperationLedger()
    private let connectionSlots = DispatchSemaphore(value: 8)
    private let staleProbeTimeout: TimeInterval
    private let beforeStaleUnlink: (() -> Void)?
    private let afterBindBeforePublish: ((String, Int32) -> Void)?
    private let afterStartupAttempt: (() -> Void)?
    private let beforeAcceptWorker: ((Int32) -> Void)?
    private let afterAcceptRetired: ((Int32, Int32) -> Void)?
    #if SWITCH_BLE_BRIDGE_TESTING
    private var failNextAcceptPoll = false

    func failNextAcceptPollForTesting() {
        lifecycleLock.lock()
        failNextAcceptPoll = true
        lifecycleLock.unlock()
    }
    #endif

    init(standardRequestTimeout: TimeInterval = 35, cardOperationTimeout: TimeInterval = 130,
         staleProbeTimeout: TimeInterval = 0.2, beforeStaleUnlink: (() -> Void)? = nil,
         afterBindBeforePublish: ((String, Int32) -> Void)? = nil,
         afterStartupAttempt: (() -> Void)? = nil,
         beforeAcceptWorker: ((Int32) -> Void)? = nil,
         afterAcceptRetired: ((Int32, Int32) -> Void)? = nil) {
        self.standardRequestTimeout = standardRequestTimeout
        self.cardOperationTimeout = cardOperationTimeout
        self.staleProbeTimeout = max(0.01, min(staleProbeTimeout, 1))
        self.beforeStaleUnlink = beforeStaleUnlink
        self.afterBindBeforePublish = afterBindBeforePublish
        self.afterStartupAttempt = afterStartupAttempt
        self.beforeAcceptWorker = beforeAcceptWorker
        self.afterAcceptRetired = afterAcceptRetired
    }

    func start(socketPath: String) {
        lifecycleLock.lock()
        guard listener < 0, pendingStartGeneration == nil else {
            lifecycleLock.unlock()
            return
        }
        listenerGeneration &+= 1
        let generation = listenerGeneration
        pendingStartGeneration = generation
        lifecycleLock.unlock()
        // A stale Unix listener can have a saturated accept queue. Probe it on
        // a private queue with a finite nonblocking deadline so application
        // startup never waits in connect(2) on the main actor.
        startupQueue.async { [weak self] in
            self?.startOnStartupQueue(socketPath: socketPath, generation: generation)
        }
    }

    private func startOnStartupQueue(socketPath: String, generation: UInt64) {
        defer {
            clearPendingStart(generation)
            // Observation only, after pending state and lifecycle locks retire.
            afterStartupAttempt?()
        }
        guard socketPath.utf8.count < MemoryLayout<sockaddr_un>.size - 4 else {
            NSLog("BLE bridge socket path too long")
            return
        }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &address.sun_path) { bytes in
            let path = Array(socketPath.utf8)
            bytes.copyBytes(from: path)
            bytes[path.count] = 0
        }
        let addressLength = socklen_t(MemoryLayout<sockaddr_un>.size)
        var existing = stat()
        if Darwin.lstat(socketPath, &existing) == 0 {
            guard (existing.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK) else {
                NSLog("BLE bridge path is not a socket; refusing to overwrite")
                return
            }
            guard existing.st_uid == Darwin.geteuid(), existing.st_mode & 0o077 == 0 else {
                NSLog("BLE bridge existing socket is not private and app-owned; preserving it")
                return
            }
            let identity = (device: UInt64(existing.st_dev), inode: UInt64(existing.st_ino))
            switch probeExistingSocket(path: socketPath, address: &address,
                                       addressLength: addressLength) {
            case .reachable:
                NSLog("BLE bridge already owned by a running app")
                return
            case .uncertain:
                NSLog("BLE bridge ownership probe was inconclusive; preserving socket")
                return
            case .stale:
                break
            }
            beforeStaleUnlink?()
            lifecycleLock.lock()
            defer { lifecycleLock.unlock() }
            guard listener < 0, pendingStartGeneration == generation,
                  listenerGeneration == generation,
                  let current = socketIdentity(at: socketPath),
                  current.device == identity.device, current.inode == identity.inode,
                  Darwin.unlink(socketPath) == 0 else { return }
        }
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard listener < 0, pendingStartGeneration == generation,
              listenerGeneration == generation else { return }
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return }
        let flags = Darwin.fcntl(fd, F_GETFL, 0)
        guard flags >= 0, Darwin.fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0 else {
            Darwin.close(fd)
            return
        }
        // Accepted sockets inherit this option. Setting it only in handle()
        // can be too late if the client closes immediately after submission.
        var noSignal: Int32 = 1
        guard Darwin.setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal,
                                socklen_t(MemoryLayout<Int32>.size)) == 0 else {
            Darwin.close(fd)
            return
        }
        var descriptorInfo = stat()
        guard Darwin.fstat(fd, &descriptorInfo) == 0,
              (descriptorInfo.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK) else {
            Darwin.close(fd)
            NSLog("BLE bridge descriptor is not a socket")
            return
        }

        // Darwin's fstat(2) identity for an AF_UNIX descriptor is not the
        // filesystem vnode identity returned by lstat(2). Publish through an
        // unguessable same-directory pathname instead: permissions and listen
        // state are finalized there, then RENAME_EXCL atomically installs it
        // without ever chmod'ing or replacing a successor at the public path.
        guard let temporaryPath = bindTemporarySocket(fd, for: socketPath),
              let identity = socketIdentity(at: temporaryPath) else {
            Darwin.close(fd)
            NSLog("BLE bridge could not bind private temporary socket")
            return
        }
        guard Darwin.chmod(temporaryPath, 0o600) == 0,
              socketIdentity(at: temporaryPath).map({
                  $0.device == identity.device && $0.inode == identity.inode
              }) == true,
              Darwin.listen(fd, 4) == 0 else {
            Darwin.close(fd)
            unlinkSocketIfOwned(temporaryPath, identity: identity)
            NSLog("BLE bridge could not bind private socket")
            return
        }
        afterBindBeforePublish?(temporaryPath, fd)
        guard Darwin.renameatx_np(AT_FDCWD, temporaryPath, AT_FDCWD, socketPath,
                                  UInt32(RENAME_EXCL)) == 0 else {
            Darwin.close(fd)
            unlinkSocketIfOwned(temporaryPath, identity: identity)
            NSLog("BLE bridge public socket appeared during startup; preserving it")
            return
        }
        guard let publishedIdentity = socketIdentity(at: socketPath),
              publishedIdentity.device == identity.device,
              publishedIdentity.inode == identity.inode else {
            Darwin.close(fd)
            unlinkSocketIfOwned(socketPath, identity: identity)
            NSLog("BLE bridge published socket identity changed; preserving successor")
            return
        }
        listener = fd
        boundSocketPath = socketPath
        boundSocketIdentity = identity
        let beforeWorker = beforeAcceptWorker
        let retired = afterAcceptRetired
        // The queued worker owns this descriptor until it actually retires.
        // stop() may revoke it, but cannot make its number reusable meanwhile.
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            beforeWorker?(fd)
            if let self {
                defer {
                    let result = self.retireListener(fd, generation: generation)
                    retired?(fd, result)
                }
                self.acceptLoop(fd, generation: generation)
            } else {
                let result = Darwin.close(fd)
                retired?(fd, result)
            }
        }
    }

    func stop() {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        listenerGeneration &+= 1
        pendingStartGeneration = nil
        guard listener >= 0 else { return }
        let fd = listener
        listener = -1
        _ = Darwin.shutdown(fd, SHUT_RDWR)
        if let path = boundSocketPath, let identity = boundSocketIdentity {
            unlinkSocketIfOwned(path, identity: identity)
        }
        boundSocketPath = nil
        boundSocketIdentity = nil
    }

    private enum ExistingSocketProbe {
        case reachable
        case stale
        case uncertain
    }

    private func probeExistingSocket(path: String,
                                     address: inout sockaddr_un,
                                     addressLength: socklen_t) -> ExistingSocketProbe {
        let deadline = ProcessInfo.processInfo.systemUptime + staleProbeTimeout
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else { return .uncertain }
        defer { Darwin.close(descriptor) }
        let flags = Darwin.fcntl(descriptor, F_GETFL, 0)
        guard flags >= 0, Darwin.fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) == 0 else {
            return .uncertain
        }
        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, addressLength)
            }
        }
        if result == 0 { return .reachable }
        let initialError = errno
        if initialError == ENOENT { return .stale }
        if initialError == ECONNREFUSED {
            return confirmRefusedSocket(path: path, deadline: deadline)
        }
        guard initialError == EINPROGRESS || initialError == EINTR || initialError == EALREADY else {
            return .uncertain
        }
        while true {
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            guard remaining > 0 else { return .uncertain }
            var item = pollfd(fd: descriptor, events: Int16(POLLOUT), revents: 0)
            let milliseconds = Int32(max(1, min(1_000, ceil(remaining * 1_000))))
            let polled = Darwin.poll(&item, 1, milliseconds)
            if polled < 0 {
                if errno == EINTR { continue }
                return .uncertain
            }
            if polled == 0 { return .uncertain }
            var socketError: Int32 = 0
            var size = socklen_t(MemoryLayout<Int32>.size)
            guard Darwin.getsockopt(descriptor, SOL_SOCKET, SO_ERROR, &socketError, &size) == 0 else {
                return .uncertain
            }
            if socketError == 0 { return .reachable }
            if socketError == ENOENT { return .stale }
            return socketError == ECONNREFUSED
                ? confirmRefusedSocket(path: path, deadline: deadline)
                : .uncertain
        }
    }

    /// Darwin may report ECONNREFUSED both for an orphaned pathname and for a
    /// live Unix listener whose accept queue is full. A refusal therefore is
    /// not stale proof by itself. Inspect same-UID socket descriptors within
    /// the same finite deadline; any matching bound descriptor preserves the
    /// pathname, while an incomplete scan is deliberately inconclusive.
    private func confirmRefusedSocket(path: String, deadline: TimeInterval) -> ExistingSocketProbe {
        guard ProcessInfo.processInfo.systemUptime < deadline else { return .uncertain }
        let requiredBytes = Darwin.proc_listpids(UInt32(PROC_UID_ONLY), UInt32(Darwin.geteuid()),
                                                 nil, 0)
        guard requiredBytes > 0 else { return .uncertain }
        let stride = MemoryLayout<pid_t>.stride
        var processIDs = [pid_t](repeating: 0, count: Int(requiredBytes) / stride + 32)
        let returnedBytes = processIDs.withUnsafeMutableBytes { buffer in
            Darwin.proc_listpids(UInt32(PROC_UID_ONLY), UInt32(Darwin.geteuid()),
                                 buffer.baseAddress, Int32(buffer.count))
        }
        guard returnedBytes >= 0 else { return .uncertain }
        let processCount = min(processIDs.count, Int(returnedBytes) / stride)
        for processID in processIDs.prefix(processCount) where processID > 0 {
            guard ProcessInfo.processInfo.systemUptime < deadline else { return .uncertain }
            let requiredFDBytes = Darwin.proc_pidinfo(processID, PROC_PIDLISTFDS, 0, nil, 0)
            if requiredFDBytes <= 0 { continue } // The process may have exited.
            let fdStride = MemoryLayout<proc_fdinfo>.stride
            var descriptors = [proc_fdinfo](repeating: proc_fdinfo(),
                                            count: Int(requiredFDBytes) / fdStride + 8)
            let returnedFDBytes = descriptors.withUnsafeMutableBytes { buffer in
                Darwin.proc_pidinfo(processID, PROC_PIDLISTFDS, 0,
                                    buffer.baseAddress, Int32(buffer.count))
            }
            if returnedFDBytes <= 0 {
                if Darwin.kill(processID, 0) == 0 { return .uncertain }
                continue
            }
            let descriptorCount = min(descriptors.count, Int(returnedFDBytes) / fdStride)
            for descriptor in descriptors.prefix(descriptorCount)
                where descriptor.proc_fdtype == PROX_FDTYPE_SOCKET {
                guard ProcessInfo.processInfo.systemUptime < deadline else { return .uncertain }
                var socketInfo = socket_fdinfo()
                let infoBytes = withUnsafeMutablePointer(to: &socketInfo) { pointer in
                    Darwin.proc_pidfdinfo(processID, descriptor.proc_fd, PROC_PIDFDSOCKETINFO,
                                          pointer, Int32(MemoryLayout<socket_fdinfo>.size))
                }
                guard infoBytes == MemoryLayout<socket_fdinfo>.size,
                      socketInfo.psi.soi_family == AF_UNIX,
                      socketInfo.psi.soi_type == SOCK_STREAM else { continue }
                if unixSocketPath(socketInfo) == path { return .reachable }
            }
        }
        return .stale
    }

    private func unixSocketPath(_ info: socket_fdinfo) -> String? {
        var address = info.psi.soi_proto.pri_un.unsi_addr.ua_sun
        guard address.sun_family == sa_family_t(AF_UNIX) else { return nil }
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        return withUnsafePointer(to: &address.sun_path) { pointer in
            pointer.withMemoryRebound(to: CChar.self, capacity: capacity) {
                String(validatingUTF8: $0)
            }
        }
    }

    private func clearPendingStart(_ generation: UInt64) {
        lifecycleLock.lock()
        if pendingStartGeneration == generation { pendingStartGeneration = nil }
        lifecycleLock.unlock()
    }

    private func socketIdentity(at path: String) -> (device: UInt64, inode: UInt64)? {
        var info = stat()
        guard Darwin.lstat(path, &info) == 0,
              (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFSOCK) else { return nil }
        return (UInt64(info.st_dev), UInt64(info.st_ino))
    }

    private func bindTemporarySocket(_ descriptor: Int32, for socketPath: String) -> String? {
        let directory = (socketPath as NSString).deletingLastPathComponent
        let maximumPathBytes = MemoryLayout<sockaddr_un>.size - 4
        // Leave room for the separator and the strict candidate-length bound.
        let nameBudget = maximumPathBytes - directory.utf8.count - 2
        // Retain at least 48 random bits; use up to the full UUID when the
        // sockaddr path budget permits it. bind(2) itself is the collision
        // arbiter and every EADDRINUSE attempt receives a fresh suffix.
        guard nameBudget >= 14 else { return nil }
        let randomHexCount = min(32, nameBudget - 2)
        for _ in 0..<8 {
            let suffix = UUID().uuidString.replacingOccurrences(of: "-", with: "")
            let candidate = (directory as NSString)
                .appendingPathComponent(".b\(suffix.prefix(randomHexCount))")
            guard candidate.utf8.count < maximumPathBytes else { return nil }
            var address = sockaddr_un()
            address.sun_family = sa_family_t(AF_UNIX)
            withUnsafeMutableBytes(of: &address.sun_path) { bytes in
                let path = Array(candidate.utf8)
                bytes.copyBytes(from: path)
                bytes[path.count] = 0
            }
            let bound = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            if bound == 0 { return candidate }
            if errno != EADDRINUSE { return nil }
        }
        return nil
    }

    private func unlinkSocketIfOwned(_ path: String, identity: (device: UInt64, inode: UInt64)) {
        guard let current = socketIdentity(at: path),
              current.device == identity.device, current.inode == identity.inode else { return }
        _ = Darwin.unlink(path)
    }

    private func retireListener(_ fd: Int32, generation: UInt64) -> Int32 {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        // Unexpected poll/accept failures must also clear the published number
        // before closing it; a later stop must never shut down a reused fd.
        if listener == fd && listenerGeneration == generation {
            listener = -1
            listenerGeneration &+= 1
            if let path = boundSocketPath, let identity = boundSocketIdentity {
                unlinkSocketIfOwned(path, identity: identity)
            }
            boundSocketPath = nil
            boundSocketIdentity = nil
        }
        return Darwin.close(fd)
    }

    private func acceptLoop(_ fd: Int32, generation: UInt64) {
        while true {
            lifecycleLock.lock()
            let active = listener == fd && listenerGeneration == generation
            lifecycleLock.unlock()
            guard active else { return }
            var item = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            #if SWITCH_BLE_BRIDGE_TESTING
            lifecycleLock.lock()
            let injectFailure = failNextAcceptPoll
            failNextAcceptPoll = false
            lifecycleLock.unlock()
            let ready: Int32
            if injectFailure {
                errno = EIO
                ready = -1
            } else {
                ready = Darwin.poll(&item, 1, 100)
            }
            #else
            let ready = Darwin.poll(&item, 1, 100)
            #endif
            if ready < 0 {
                if errno == EINTR { continue }
                return
            }
            if ready == 0 { continue }
            if item.revents & Int16(POLLERR | POLLHUP | POLLNVAL) != 0 { return }
            guard item.revents & Int16(POLLIN) != 0 else { return }
            let client = Darwin.accept(fd, nil, nil)
            if client < 0 {
                if errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK { continue }
                return
            }
            lifecycleLock.lock()
            let current = listener == fd && listenerGeneration == generation
            lifecycleLock.unlock()
            guard current else { Darwin.close(client); return }
            // Darwin can inherit listener flags. Preserve the handler's existing
            // blocking read/write plus five-second socket timeout semantics.
            let flags = Darwin.fcntl(client, F_GETFL, 0)
            guard flags >= 0, Darwin.fcntl(client, F_SETFL, flags & ~O_NONBLOCK) == 0 else {
                Darwin.close(client)
                continue
            }
            guard connectionSlots.wait(timeout: .now()) == .success else {
                Darwin.close(client)
                continue
            }
            DispatchQueue.global(qos: .userInitiated).async { [self] in
                defer { Darwin.close(client); connectionSlots.signal() }
                handle(client)
            }
        }
    }

    private func handle(_ fd: Int32) {
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        withUnsafePointer(to: &timeout) {
            _ = Darwin.setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, $0, socklen_t(MemoryLayout<timeval>.size))
            _ = Darwin.setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, $0, socklen_t(MemoryLayout<timeval>.size))
        }
        var noSignal: Int32 = 1
        _ = Darwin.setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while data.count <= maxRequestBytes {
            let capacity = buffer.count
            let count = buffer.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress, capacity) }
            if count <= 0 { return }
            data.append(contentsOf: buffer.prefix(count))
            if let newline = data.firstIndex(of: 10) {
                data = data.prefix(upTo: newline)
                break
            }
        }
        guard data.count <= maxRequestBytes,
              let request = try? JSONDecoder().decode(Request.self, from: data),
              constantTimeEquals(request.token, LocalBackendAuthentication.appToken) else {
            send(Response(ok: false, errorCode: "ble_unauthorized"), fd: fd)
            return
        }
        if request.action == "operation-status" {
            let snapshot = operations.status(operationID: request.operationID ?? "",
                                             bridgeID: request.bridgeID ?? "")
            send(proofResponse(snapshot), fd: fd)
            return
        }
        if request.action == "apdu" || request.action == "reset" {
            handleCard(request, fd: fd)
            return
        }
        let semaphore = DispatchSemaphore(value: 0)
        let answerLock = NSLock()
        var answer = Response(ok: false, errorCode: "ble_timeout")
        let complete: (Response) -> Void = { value in
            answerLock.lock()
            answer = value
            answerLock.unlock()
            semaphore.signal()
        }
        DispatchQueue.main.async { [self] in
            let manager = BluetoothReaderManager.shared
            manager.physicalConnectionClosed = { [weak self] identity in
                self?.operations.confirmDisconnected(reader: identity.reader,
                    connectionID: identity.connectionID, operationID: identity.closeOperationID)
            }
            switch request.action {
            case "status":
                let devices = manager.devices.map { Device(id: $0.key.uuidString, name: $0.value) }
                let state = String(describing: manager.state)
                let identity = manager.connectionIdentityForBridge()
                let trackedConnection = identity.flatMap { identity in
                    self.operations.registerConnection(reader: identity.reader,
                        connectionID: identity.connectionID, operationID: identity.closeOperationID) ? identity : nil
                }
                if let id = manager.connectedID, manager.state == .connected {
                    complete(Response(ok: true, reader: "BLE:" + id.uuidString,
                                      devices: devices, state: state,
                                      bridgeID: self.operations.bridgeID, operationTracking: true,
                                      connectionID: trackedConnection?.connectionID,
                                      closeOperationID: trackedConnection?.closeOperationID))
                } else {
                    complete(Response(ok: true, devices: devices, state: state,
                                      errorCode: manager.errorCode?.rawValue,
                                      bridgeID: self.operations.bridgeID, operationTracking: true))
                }
            case "scan":
                manager.scan()
                complete(Response(ok: true))
            case "connect":
                guard let reader = request.reader,
                      let id = UUID(uuidString: reader.replacingOccurrences(of: "BLE:", with: "",
                                                                            options: [.anchored, .caseInsensitive])),
                      manager.devices[id] != nil else {
                    complete(Response(ok: false, errorCode: "ble_reader_not_found"))
                    break
                }
                manager.connect(id)
                if manager.state == .connecting {
                    complete(Response(ok: true, state: "connecting"))
                } else {
                    complete(Response(ok: false,
                                      errorCode: manager.errorCode?.rawValue ?? "ble_connect_rejected"))
                }
            case "disconnect":
                guard let reader = request.reader,
                      let id = UUID(uuidString: reader.replacingOccurrences(of: "BLE:", with: "",
                                                                            options: [.anchored, .caseInsensitive])),
                      manager.matchesLifecycleTarget(id) else {
                    complete(Response(ok: false, errorCode: "ble_stale_reader_binding"))
                    break
                }
                manager.disconnect()
                complete(Response(ok: true))
            default:
                complete(Response(ok: false, errorCode: "ble_unknown_action"))
            }
        }
        if semaphore.wait(timeout: .now() + standardRequestTimeout) == .timedOut {
            answerLock.lock()
            answer = Response(ok: false, errorCode: "ble_timeout")
            answerLock.unlock()
        }
        answerLock.lock()
        let finalAnswer = answer
        answerLock.unlock()
        send(finalAnswer, fd: fd)
    }

    private func handleCard(_ request: Request, fd: Int32) {
        let tracked = request.operationID != nil || request.bridgeID != nil
        let operationID = request.operationID ?? (tracked ? "" : UUID().uuidString)
        let bridgeID = request.bridgeID ?? (tracked ? "" : operations.bridgeID)
        let fingerprint = ReaderBLEOperationLedger.Fingerprint(action: request.action,
                                                               reader: request.reader, hex: request.hex, connectionID: request.connectionID)
        var rejection: ReaderBLEOperationLedger.Result?
        let payload = request.action == "apdu" ? Data(hexString: request.hex ?? "") : nil
        if request.action == "apdu" && (payload == nil || (tracked && payload?.isEmpty == true)) {
            rejection = .init(ok: false, errorCode: "ble_invalid_apdu")
        }
        if tracked && !validReader(request.reader) {
            rejection = .init(ok: false, errorCode: "ble_reader_changed")
        }
        switch operations.admit(operationID: operationID, bridgeID: bridgeID,
                                 fingerprint: fingerprint, rejection: rejection) {
        case .reply(let snapshot):
            send(cardResponse(snapshot, tracked: tracked), fd: fd)
        case .dispatch(let ticket):
            let completion = DispatchSemaphore(value: 0)
            DispatchQueue.main.async { [self] in
                // A duplicate socket request never acquires a second ticket.
                // This CAS also fences a superseded/cancelled queued closure.
                guard operations.begin(ticket) else { completion.signal(); return }
                let finish: (ReaderBLEOperationLedger.Result) -> Void = { result in
                    self.operations.finish(ticket, result: result)
                    completion.signal()
                }
                let manager = BluetoothReaderManager.shared
                guard manager.isCardChannelReady(for: request.reader),
                      request.connectionID == nil || manager.connectionIdentityForBridge()?.connectionID == request.connectionID else {
                    finish(.init(ok: false, errorCode: request.reader == nil
                                 ? BluetoothReaderErrorCode.disconnected.rawValue : "ble_reader_changed"))
                    return
                }
                let cardFinished: (Result<Data, Error>) -> Void = { result in
                    switch result {
                    case .success(let bytes): finish(.init(ok: true, hex: bytes.hexString))
                    case .failure(let error):
                        let code = (error as? BluetoothReaderTransportError)?.code.rawValue ?? "ble_exchange_failed"
                        finish(.init(ok: false, errorCode: code))
                    }
                }
                manager.performCardOperationWithCompletionProof({ confirmedCompletion in
                    if request.action == "reset" {
                        manager.powerCycle(completion: confirmedCompletion)
                    } else if let payload {
                        manager.exchangeAPDU(payload, completion: confirmedCompletion)
                    }
                }, completion: cardFinished)
            }
            _ = completion.wait(timeout: .now() + cardOperationTimeout)
            // Timeout is only the end of this socket's wait. Never fabricate a
            // terminal reply while queued/running physical work remains owned.
            let snapshot = operations.status(operationID: operationID, bridgeID: bridgeID)
            send(cardResponse(snapshot, tracked: tracked), fd: fd)
        }
    }

    private func validReader(_ reader: String?) -> Bool {
        guard let reader, reader.hasPrefix("BLE:") else { return false }
        return UUID(uuidString: String(reader.dropFirst(4))) != nil
    }

    private func cardResponse(_ snapshot: ReaderBLEOperationLedger.Snapshot, tracked: Bool) -> Response {
        if tracked { return proofResponse(snapshot) }
        // Legacy clients keep their original response fields and timeout code;
        // their timed-out jobs still occupy the common physical ledger.
        return Response(ok: snapshot.result.ok, hex: snapshot.result.hex,
                        errorCode: snapshot.terminal ? snapshot.result.errorCode : "ble_timeout")
    }

    private func proofResponse(_ snapshot: ReaderBLEOperationLedger.Snapshot) -> Response {
        Response(ok: snapshot.result.ok, reader: snapshot.reader, hex: snapshot.result.hex, errorCode: snapshot.result.errorCode,
                 operationID: snapshot.operationID, bridgeID: snapshot.bridgeID, terminal: snapshot.terminal,
                 connectionID: snapshot.connectionID, closed: snapshot.closed)
    }

    private func send(_ response: Response, fd: Int32) {
        guard var bytes = try? JSONEncoder().encode(response) else { return }
        bytes.append(10)
        bytes.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            var sent = 0
            while sent < raw.count {
                let count = Darwin.write(fd, base.advanced(by: sent), raw.count - sent)
                if count <= 0 { return }
                sent += count
            }
        }
    }

    private func constantTimeEquals(_ first: String, _ second: String) -> Bool {
        let a = Array(first.utf8), b = Array(second.utf8)
        guard a.count == b.count else { return false }
        var difference: UInt8 = 0
        for index in a.indices { difference |= a[index] ^ b[index] }
        return difference == 0
    }

    private struct Request: Decodable {
        let token: String
        let action: String
        let hex: String?
        let reader: String?
        let operationID: String?
        let bridgeID: String?
        let connectionID: String?
        enum CodingKeys: String, CodingKey {
            case token, action, hex, reader
            case operationID = "operation_id", bridgeID = "bridge_id", connectionID = "connection_id"
        }
    }
    private struct Device: Encodable { let id: String; let name: String }
    private struct Response: Encodable {
        let ok: Bool
        var reader: String? = nil
        var devices: [Device]? = nil
        var state: String? = nil
        var hex: String? = nil
        var errorCode: String? = nil
        var operationID: String? = nil
        var bridgeID: String? = nil
        var terminal: Bool? = nil
        var operationTracking: Bool? = nil
        var connectionID: String? = nil
        var closeOperationID: String? = nil
        var closed: Bool? = nil
        enum CodingKeys: String, CodingKey {
            case ok, reader, devices, state, hex, errorCode, terminal
            case operationID = "operation_id", bridgeID = "bridge_id"
            case operationTracking = "operation_tracking"
            case connectionID = "connection_id", closeOperationID = "close_operation_id", closed
        }
    }
}

private extension Data {
    init?(hexString: String) {
        guard hexString.count % 2 == 0, hexString.count <= 131_070 else { return nil }
        self.init(capacity: hexString.count / 2)
        var index = hexString.startIndex
        while index < hexString.endIndex {
            let next = hexString.index(index, offsetBy: 2)
            guard let byte = UInt8(hexString[index..<next], radix: 16) else { return nil }
            append(byte)
            index = next
        }
    }
    var hexString: String { map { String(format: "%02x", $0) }.joined() }
}
