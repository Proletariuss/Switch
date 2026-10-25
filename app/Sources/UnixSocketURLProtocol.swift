import Foundation
import Darwin

/// 让 URLSession 支持 http+unix:// 协议，通过 Unix domain socket 访问本机后端。
///
/// URL 形如 http+unix://proletarius/api/status，其中 host 部分无实际含义，
/// 真实 socket 路径由 `socketPath` 静态属性提供。
final class UnixSocketURLProtocol: URLProtocol, @unchecked Sendable {
    /// 由 BackendProcess 在初始化时设置
    private final class SocketPathStorage: @unchecked Sendable {
        private let lock = NSLock()
        private var value = ""

        func read() -> String {
            lock.lock()
            defer { lock.unlock() }
            return value
        }

        func write(_ newValue: String) {
            lock.lock()
            value = newValue
            lock.unlock()
        }
    }

    private static let socketPathStorage = SocketPathStorage()
    static var socketPath: String {
        get { socketPathStorage.read() }
        set { socketPathStorage.write(newValue) }
    }

    private let socketLock = NSLock()
    private var socketFD: Int32 = -1
    private var stopped = false
    private let ioQueue = DispatchQueue(label: "unix-socket-protocol")

#if UNIX_SOCKET_TRANSPORT_TEST_HOOKS
    /// Instance-owned compile-only seams; install before starting this request.
    struct TransportTestHooks: Sendable {
        var afterOwnedClose: (@Sendable (Int32) -> Void)? = nil
        var connectResult: (@Sendable (Int32) -> (result: Int32, error: Int32))? = nil
        var pendingCancellationCheck: (@Sendable (Int32) -> Void)? = nil
    }
    private var transportTestHooks: TransportTestHooks?
    private var transportTestStarted = false

    func installTransportTestHooks(_ hooks: TransportTestHooks) {
        socketLock.lock()
        defer { socketLock.unlock() }
        precondition(!transportTestStarted, "install hooks before startLoading")
        transportTestHooks = hooks
    }

    /// Called off the I/O queue: the prior perform closure has really returned.
    func waitForTransportTestWorkerExit() {
        ioQueue.sync {}
    }
#endif

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.scheme == "http+unix"
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest {
        request
    }

    override class func requestIsCacheEquivalent(_ a: URLRequest, to b: URLRequest) -> Bool {
        false
    }

    override func startLoading() {
#if UNIX_SOCKET_TRANSPORT_TEST_HOOKS
        socketLock.lock()
        transportTestStarted = true
        socketLock.unlock()
#endif
        let request = request
        let socketPath = Self.socketPath
        ioQueue.async { [weak self] in
            guard let self else { return }
            self.perform(request: request, socketPath: socketPath)
        }
    }

    override func stopLoading() {
        socketLock.lock()
        stopped = true
        // Only the I/O queue closes this descriptor. Keep shutdown under the
        // lock so the owner cannot close it and let its number be reused first.
        if socketFD >= 0 { _ = Darwin.shutdown(socketFD, SHUT_RDWR) }
        socketLock.unlock()
    }

    // MARK: - 请求执行

    private func perform(request: URLRequest, socketPath: String) {
        let interval = request.timeoutInterval.isFinite && request.timeoutInterval > 0
            ? request.timeoutInterval : 60
        let deadline = ProcessInfo.processInfo.systemUptime + interval
        guard !isStopped else { return }
        guard let fd = Self.makeSocket() else {
            fail(URLError(.cannotConnectToHost))
            return
        }
        guard registerSocket(fd) else { return }
        defer { closeSocketIfOwned(fd) }

        let status: Int
        let headers: [String: String]
        let body: Data?
        do {
#if UNIX_SOCKET_TRANSPORT_TEST_HOOKS
            if let connectOverride = transportTestHooks?.connectResult {
                // Only the syscall outcome is injected. Pending classification,
                // poll, cancellation and owned-fd cleanup are the real path.
                let outcome = connectOverride(fd)
                try Self.completeConnect(fd: fd, result: outcome.result, connectError: outcome.error,
                                         deadline: deadline, cancelled: {
                    self.transportTestHooks?.pendingCancellationCheck?(fd)
                    return self.isStopped
                })
            } else {
                try Self.connect(fd: fd, path: socketPath, deadline: deadline, cancelled: { self.isStopped })
            }
#else
            try Self.connect(fd: fd, path: socketPath, deadline: deadline, cancelled: { self.isStopped })
#endif
            guard !isStopped else { throw URLError(.cancelled) }
            guard ProcessInfo.processInfo.systemUptime < deadline else { throw URLError(.timedOut) }
            let wire = try buildRequestData(request, deadline: deadline)
            var sent = 0
            while sent < wire.count {
                try Self.wait(fd: fd, events: Int16(POLLOUT), deadline: deadline, cancelled: { self.isStopped })
                let n = wire.withUnsafeBytes { ptr -> Int in
                    guard let baseAddress = ptr.baseAddress else { return -1 }
                    return Darwin.send(fd, baseAddress.advanced(by: sent), wire.count - sent, 0)
                }
                if n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) { continue }
                guard n > 0 else { throw URLError(.networkConnectionLost) }
                sent += n
            }
            (status, headers, body) = try readResponse(fd: fd, deadline: deadline)
        } catch let error as URLError {
            fail(error)
            return
        } catch {
            fail(URLError(.badServerResponse))
            return
        }

        let url = request.url ?? URL(string: "http+unix://proletarius/")!
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: headers
        )
        guard let response, !isStopped else { return }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        if let body, !body.isEmpty {
            client?.urlProtocol(self, didLoad: body)
        }
        client?.urlProtocolDidFinishLoading(self)
    }

    private func fail(_ error: URLError) {
        guard !isStopped else { return }
        client?.urlProtocol(self, didFailWithError: error)
    }

    private var isStopped: Bool {
        socketLock.lock()
        defer { socketLock.unlock() }
        return stopped
    }

    private func registerSocket(_ fd: Int32) -> Bool {
        socketLock.lock()
        guard !stopped else {
            socketLock.unlock()
            Darwin.close(fd)
            return false
        }
        socketFD = fd
        socketLock.unlock()
        return true
    }

    private func closeSocketIfOwned(_ fd: Int32) {
        socketLock.lock()
        let shouldClose = socketFD == fd
        if shouldClose { socketFD = -1 }
        socketLock.unlock()
        if shouldClose {
            Darwin.close(fd)
#if UNIX_SOCKET_TRANSPORT_TEST_HOOKS
            transportTestHooks?.afterOwnedClose?(fd)
#endif
        }
    }

    // MARK: - 请求构建

    private static let maximumRequestHeaderBytes = 64 * 1024
    private static let maximumRequestBodyBytes = 64 * 1024 * 1024

    private func buildRequestData(_ request: URLRequest, deadline: TimeInterval) throws -> Data {
        guard let url = request.url,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw URLError(.badURL)
        }
        let method = request.httpMethod ?? "GET"
        guard !method.isEmpty, method.utf8.count < Self.maximumRequestHeaderBytes,
              method.utf8.allSatisfy({ Self.isHTTPToken($0) }) else {
            throw URLError(.badURL)
        }
        var path = components.percentEncodedPath.isEmpty ? "/" : components.percentEncodedPath
        guard path.utf8.count <= Self.maximumRequestHeaderBytes else { throw URLError(.badURL) }
        if let query = components.percentEncodedQuery {
            guard query.utf8.count < Self.maximumRequestHeaderBytes - path.utf8.count else {
                throw URLError(.badURL)
            }
            path += "?\(query)"
        }
        guard path.utf8.allSatisfy({ (33...126).contains($0) }) else { throw URLError(.badURL) }
        guard path.utf8.count < Self.maximumRequestHeaderBytes - method.utf8.count else {
            throw URLError(.badURL)
        }

        var lines = ["\(method) \(path) HTTP/1.1"]
        lines.append("Host: proletarius.local")
        lines.append("Connection: close")
        var headerBytes = lines.reduce(0) { $0 + $1.utf8.count + 2 }
        guard headerBytes <= Self.maximumRequestHeaderBytes else { throw URLError(.badURL) }

        let defaultHeaders = Set(["host", "connection", "content-length"])
        for (key, value) in request.allHTTPHeaderFields ?? [:] {
            guard !key.isEmpty, key.utf8.allSatisfy({ Self.isHTTPToken($0) }),
                  !value.utf8.contains(where: { $0 < 32 && $0 != 9 || $0 == 127 }) else {
                throw URLError(.badURL)
            }
            guard !defaultHeaders.contains(key.lowercased()) else { continue }
            // Count before interpolation/joining so oversized caller headers
            // cannot create a second unbounded serialized copy.
            let remaining = Self.maximumRequestHeaderBytes - headerBytes
            guard remaining >= 4, key.utf8.count <= remaining - 4,
                  value.utf8.count <= remaining - 4 - key.utf8.count else {
                throw URLError(.badURL)
            }
            headerBytes += key.utf8.count + value.utf8.count + 4
            lines.append("\(key): \(value)")
        }

        // A caller-supplied InputStream may block inside read() indefinitely.
        // It cannot be interrupted safely by stopLoading or the socket deadline.
        guard request.httpBodyStream == nil else { throw URLError(.badURL) }
        let body = request.httpBody ?? Data()
        guard body.count <= Self.maximumRequestBodyBytes else {
            throw URLError(.dataLengthExceedsMaximum)
        }
        if !body.isEmpty {
            let lengthLine = "Content-Length: \(body.count)"
            guard lengthLine.utf8.count + 2 <= Self.maximumRequestHeaderBytes - headerBytes else {
                throw URLError(.badURL)
            }
            lines.append(lengthLine)
        }

        var data = Data(lines.joined(separator: "\r\n").utf8)
        // Include the final CRLFCRLF in the on-wire header budget.
        guard data.count <= Self.maximumRequestHeaderBytes - 4 else { throw URLError(.badURL) }
        guard !isStopped else { throw URLError(.cancelled) }
        guard ProcessInfo.processInfo.systemUptime < deadline else { throw URLError(.timedOut) }
        data.append(contentsOf: [0x0D, 0x0A, 0x0D, 0x0A])
        if !body.isEmpty { data.append(body) }
        return data
    }

    // MARK: - 响应解析

    // Limits apply before allocation or integer addition. Raw chunk framing is
    // consumed incrementally, so a peer cannot grow an unbounded wire buffer.
    private static let maximumHeaderBytes = 64 * 1024
    private static let maximumBodyBytes = 64 * 1024 * 1024
    private static let maximumChunkLineBytes = 8 * 1024

    private func readResponse(fd: Int32, deadline: TimeInterval) throws -> (Int, [String: String], Data?) {
        var buffer = Data()
        var scratch = [UInt8](repeating: 0, count: 16 * 1024)
        let crlf = Data([13, 10])
        let headerBoundary = Data([13, 10, 13, 10])
        func readMore() throws {
            while true {
                try Self.wait(fd: fd, events: Int16(POLLIN), deadline: deadline, cancelled: { self.isStopped })
                let n = Darwin.recv(fd, &scratch, scratch.count, 0)
                if n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) { continue }
                guard n > 0 else { throw URLError(.badServerResponse) }
                buffer.append(scratch, count: n)
                return
            }
        }
        func take(_ count: Int) throws -> Data {
            while buffer.count < count { try readMore() }
            let result = Data(buffer.prefix(count))
            buffer.removeFirst(count)
            return result
        }
        func line(limit: Int) throws -> Data {
            while true {
                if let boundary = buffer.range(of: crlf) {
                    let length = buffer.distance(from: buffer.startIndex, to: boundary.lowerBound)
                    guard length <= limit else { throw URLError(.badServerResponse) }
                    let result = Data(buffer[..<boundary.lowerBound])
                    buffer.removeSubrange(buffer.startIndex..<boundary.upperBound)
                    return result
                }
                guard buffer.count <= limit + 1 else { throw URLError(.badServerResponse) }
                try readMore()
            }
        }

        var headerRange: Range<Data.Index>
        while true {
            if let boundary = buffer.range(of: headerBoundary) {
                guard buffer.distance(from: buffer.startIndex, to: boundary.lowerBound) <= Self.maximumHeaderBytes else {
                    throw URLError(.badServerResponse)
                }
                headerRange = boundary
                break
            }
            guard buffer.count <= Self.maximumHeaderBytes + 3 else { throw URLError(.badServerResponse) }
            try readMore()
        }
        guard let head = String(data: buffer[..<headerRange.lowerBound], encoding: .utf8) else {
            throw URLError(.badServerResponse)
        }
        buffer.removeSubrange(buffer.startIndex..<headerRange.upperBound)
        let lines = head.components(separatedBy: "\r\n")
        let statusParts = lines[0].split(separator: " ", maxSplits: 2)
        guard statusParts.count >= 2,
              ["HTTP/1.0", "HTTP/1.1"].contains(String(statusParts[0])),
              statusParts[1].count == 3, let status = Int(statusParts[1]), (100...599).contains(status) else {
            throw URLError(.badServerResponse)
        }
        var headers: [String: String] = [:]
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else { throw URLError(.badServerResponse) }
            let key = line[..<colon].lowercased()
            guard !key.isEmpty, key.utf8.allSatisfy({ Self.isHTTPToken($0) }) else {
                throw URLError(.badServerResponse)
            }
            let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            guard !value.utf8.contains(where: { $0 < 32 && $0 != 9 || $0 == 127 }) else {
                throw URLError(.badServerResponse)
            }
            if let previous = headers[key], key == "content-length" {
                guard previous == value else { throw URLError(.badServerResponse) }
            } else if let previous = headers[key], key == "transfer-encoding" {
                headers[key] = previous + "," + value
                continue
            }
            headers[key] = value
        }
        if let encoding = headers["transfer-encoding"] {
            // This local bridge implements chunked framing only; ambiguous
            // length/encoding combinations are rejected before consuming body.
            guard encoding.lowercased() == "chunked", headers["content-length"] == nil else {
                throw URLError(.badServerResponse)
            }
            var body = Data()
            while true {
                let sizeData = try line(limit: Self.maximumChunkLineBytes)
                guard let sizeLine = String(data: sizeData, encoding: .ascii),
                      !sizeData.contains(where: { $0 < 32 && $0 != 9 || $0 == 127 }) else {
                    throw URLError(.badServerResponse)
                }
                let sizeHex = sizeLine.split(separator: ";", maxSplits: 1, omittingEmptySubsequences: false)[0]
                guard !sizeHex.isEmpty,
                      sizeHex.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
                      let size = Int(sizeHex, radix: 16), size <= Self.maximumBodyBytes - body.count else {
                    throw URLError(.badServerResponse)
                }
                if size == 0 {
                    var trailerBytes = 0
                    while true {
                        let trailer = try line(limit: Self.maximumHeaderBytes - trailerBytes)
                        trailerBytes += trailer.count + 2
                        guard trailerBytes <= Self.maximumHeaderBytes else { throw URLError(.badServerResponse) }
                        if trailer.isEmpty { return (status, headers, body) }
                        guard let text = String(data: trailer, encoding: .ascii),
                              let colon = text.firstIndex(of: ":"),
                              !text[..<colon].isEmpty,
                              text[..<colon].utf8.allSatisfy({ Self.isHTTPToken($0) }),
                              !text.utf8.contains(where: { $0 < 32 && $0 != 9 || $0 == 127 }) else {
                            throw URLError(.badServerResponse)
                        }
                    }
                }
                // Read bounded slices even when a chunk declares a large body.
                var remaining = size
                while remaining > 0 {
                    let count = min(remaining, 16 * 1024)
                    body.append(try take(count))
                    remaining -= count
                }
                guard try take(2) == crlf else { throw URLError(.badServerResponse) }
            }
        }
        if let lengthText = headers["content-length"] {
            guard !lengthText.isEmpty, lengthText.utf8.allSatisfy({ (48...57).contains($0) }),
                  let length = Int(lengthText), length <= Self.maximumBodyBytes else {
                throw URLError(.badServerResponse)
            }
            var body = Data()
            while body.count < length { body.append(try take(min(length - body.count, 16 * 1024))) }
            return (status, headers, body)
        }
        // Preserve the bridge's existing no-framing/no-body behavior.
        return (status, headers, nil)
    }

    private static func isHTTPToken(_ byte: UInt8) -> Bool {
        (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
            || Array("!#$%&'*+-.^_`|~".utf8).contains(byte)
    }

    // MARK: - Unix socket connection

    /// Probes ownership without unlinking a socket belonging to a live backend.
    static func isSocketReachable(_ path: String) -> Bool {
        guard let fd = makeSocket() else { return false }
        defer { Darwin.close(fd) }
        do {
            try connect(fd: fd, path: path, deadline: ProcessInfo.processInfo.systemUptime + 0.25, cancelled: { false })
            return true
        } catch { return false }
    }

    private static func makeSocket() -> Int32? {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        var noSignal: Int32 = 1
        guard setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size)) == 0,
              fcntl(fd, F_SETFL, O_NONBLOCK) == 0 else {
            Darwin.close(fd)
            return nil
        }
        return fd
    }

    private static func connect(fd: Int32, path: String, deadline: TimeInterval, cancelled: () -> Bool) throws {
        let pathBytes = Array(path.utf8)
        var addr = sockaddr_un()
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        guard !pathBytes.isEmpty, !pathBytes.contains(0), pathBytes.count < capacity else {
            throw URLError(.cannotConnectToHost)
        }
        addr.sun_family = sa_family_t(AF_UNIX)
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutablePointer(to: &addr.sun_path) { pathPtr in
            let base = UnsafeMutableRawPointer(pathPtr)
            for (i, byte) in pathBytes.enumerated() { base.storeBytes(of: byte, toByteOffset: i, as: UInt8.self) }
            base.storeBytes(of: 0, toByteOffset: pathBytes.count, as: UInt8.self)
        }
        guard !cancelled() else { throw URLError(.cancelled) }
        let result = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        // Snapshot errno before another helper can change it.
        let connectError = errno
        try completeConnect(fd: fd, result: result, connectError: connectError,
                            deadline: deadline, cancelled: cancelled)
    }

    private static func completeConnect(fd: Int32, result: Int32, connectError: Int32,
                                        deadline: TimeInterval, cancelled: () -> Bool) throws {
        if result == 0 { return }
        guard connectError == EINPROGRESS || connectError == EALREADY || connectError == EINTR else {
            throw URLError(.cannotConnectToHost)
        }
        try wait(fd: fd, events: Int16(POLLOUT), deadline: deadline, cancelled: cancelled)
        var socketError: Int32 = 0
        var length = socklen_t(MemoryLayout<Int32>.size)
        guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &socketError, &length) == 0, socketError == 0 else {
            throw URLError(.cannotConnectToHost)
        }
    }

    private static func wait(fd: Int32, events: Int16, deadline: TimeInterval, cancelled: () -> Bool) throws {
        while true {
            guard !cancelled() else { throw URLError(.cancelled) }
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            guard remaining > 0 else { throw URLError(.timedOut) }
            // shutdown need not wake an unconnected AF_UNIX socket. Short
            // bounded polls also make cancellation during connect responsive.
            let milliseconds = Int32(max(1, min(50, ceil(remaining * 1000))))
            var descriptor = pollfd(fd: fd, events: events, revents: 0)
            let result = Darwin.poll(&descriptor, 1, milliseconds)
            if result < 0 && errno == EINTR { continue }
            guard result >= 0 else { throw URLError(.networkConnectionLost) }
            if result > 0 {
                guard !cancelled() else { throw URLError(.cancelled) }
                guard descriptor.revents & Int16(POLLNVAL) == 0 else { throw URLError(.networkConnectionLost) }
                return // recv/send/SO_ERROR classifies EOF, hangup and errors.
            }
        }
    }
}
