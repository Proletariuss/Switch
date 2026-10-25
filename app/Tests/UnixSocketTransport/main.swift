import Foundation
import Darwin

struct TestFailure: Error { let message: String }
func check(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw TestFailure(message: message) }
}

private final class Client: NSObject, URLProtocolClient, @unchecked Sendable {
    let finished = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var storedError: URLError.Code?
    private var storedBody = Data()
    private var storedCallbacks = 0
    var error: URLError.Code? { lock.lock(); defer { lock.unlock() }; return storedError }
    var body: Data { lock.lock(); defer { lock.unlock() }; return storedBody }
    var callbacks: Int { lock.lock(); defer { lock.unlock() }; return storedCallbacks }
    func urlProtocol(_ protocol: URLProtocol, didFailWithError error: Error) {
        lock.lock(); storedError = (error as? URLError)?.code; storedCallbacks += 1; lock.unlock(); finished.signal()
    }
    func urlProtocol(_ protocol: URLProtocol, didLoad data: Data) {
        lock.lock(); storedBody.append(data); storedCallbacks += 1; lock.unlock()
    }
    func urlProtocolDidFinishLoading(_ protocol: URLProtocol) {
        lock.lock(); storedCallbacks += 1; lock.unlock(); finished.signal()
    }
    func urlProtocol(_ protocol: URLProtocol, didReceive response: URLResponse, cacheStoragePolicy policy: URLCache.StoragePolicy) {
        lock.lock(); storedCallbacks += 1; lock.unlock()
    }
    func urlProtocol(_ protocol: URLProtocol, wasRedirectedTo request: URLRequest, redirectResponse: URLResponse) {}
    func urlProtocol(_ protocol: URLProtocol, cachedResponseIsValid cachedResponse: CachedURLResponse) {}
    func urlProtocol(_ protocol: URLProtocol, didReceive challenge: URLAuthenticationChallenge) {}
    func urlProtocol(_ protocol: URLProtocol, didCancel challenge: URLAuthenticationChallenge) {}
}

private final class Server: @unchecked Sendable {
    let path: String
    private let directory: String
    private let listener: Int32
    private let finished = DispatchSemaphore(value: 0)
    let accepted = DispatchSemaphore(value: 0)
    init(_ handler: @escaping @Sendable (Int32) -> Void) throws {
        directory = "/tmp/switch-transport-" + UUID().uuidString.prefix(8)
        try FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: false,
                                               attributes: [.posixPermissions: 0o700])
        path = directory + "/test.sock"
        listener = socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw TestFailure(message: "socket failed") }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let bytes = Array(path.utf8) + [0]
        withUnsafeMutablePointer(to: &address.sun_path) { UnsafeMutableRawPointer($0).copyMemory(from: bytes, byteCount: bytes.count) }
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0, listen(listener, 1) == 0 else { throw TestFailure(message: "bind/listen failed") }
        DispatchQueue.global().async { [self] in
            defer { finished.signal() }
            let peer = accept(listener, nil, nil)
            guard peer >= 0 else { return }
            defer { Darwin.close(peer) }
            var noSignal: Int32 = 1
            _ = setsockopt(peer, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
            accepted.signal()
            handler(peer)
        }
    }
    func close() throws {
        try check(finished.wait(timeout: .now() + 3) == .success, "fixture did not finish")
        Darwin.close(listener)
        try FileManager.default.removeItem(atPath: directory)
    }
}

private func sendText(_ fd: Int32, _ value: String) {
    let data = Data(value.utf8)
    data.withUnsafeBytes { ptr in
        var offset = 0
        while offset < ptr.count {
            let n = send(fd, ptr.baseAddress!.advanced(by: offset), ptr.count - offset, 0)
            if n <= 0 { break }
            offset += n
        }
    }
}
private func readRequest(_ fd: Int32) {
    var scratch = [UInt8](repeating: 0, count: 4096)
    _ = recv(fd, &scratch, scratch.count, 0)
}
private func launch(path: String, timeout: TimeInterval = 1, body: Data? = nil) -> (UnixSocketURLProtocol, Client) {
    UnixSocketURLProtocol.socketPath = path
    var request = URLRequest(url: URL(string: "http+unix://local/api/test?value=1")!)
    request.timeoutInterval = timeout
    request.httpMethod = body == nil ? "GET" : "POST"
    request.httpBody = body
    let client = Client()
    let transport = UnixSocketURLProtocol(request: request, cachedResponse: nil, client: client)
    transport.startLoading()
    return (transport, client)
}
private func launchRequest(_ request: URLRequest, path: String) -> (UnixSocketURLProtocol, Client) {
    UnixSocketURLProtocol.socketPath = path
    let client = Client()
    let transport = UnixSocketURLProtocol(request: request, cachedResponse: nil, client: client)
    transport.startLoading()
    return (transport, client)
}

private final class RequestCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var value = Data()
    func save(_ data: Data) { lock.lock(); value = data; lock.unlock() }
    func read() -> Data { lock.lock(); defer { lock.unlock() }; return value }
}

private final class StalledInputStream: InputStream {
    let entered = DispatchSemaphore(value: 0)
    let release = DispatchSemaphore(value: 0)
    init() { super.init(data: Data()) }
    override var hasBytesAvailable: Bool { true }
    override func open() {}
    override func close() {}
    override func read(_ buffer: UnsafeMutablePointer<UInt8>, maxLength len: Int) -> Int {
        entered.signal()
        _ = release.wait(timeout: .now() + 2)
        return 0
    }
}

private func requestConstructionTests() throws {
    func runRejected(_ configure: (inout URLRequest) -> Void, expected: URLError.Code) throws {
        let server = try Server { fd in
            readRequest(fd)
        }
        var request = URLRequest(url: URL(string: "http+unix://local/api/test")!)
        request.timeoutInterval = 1
        configure(&request)
        let (transport, client) = launchRequest(request, path: server.path)
        try check(client.finished.wait(timeout: .now() + 1.5) == .success, "request validation did not terminate")
        try check(client.error == expected, "invalid request accepted or misclassified: \(String(describing: client.error))")
        withExtendedLifetime(transport) {}
        try server.close()
    }

    try runRejected({ $0.httpMethod = "GET\r\nInjected: yes" }, expected: .badURL)
    try runRejected({ $0.allHTTPHeaderFields = ["Bad\r\nName": "ok"] }, expected: .badURL)

    let capture = RequestCapture()
    let pathServer = try Server { fd in
        var scratch = [UInt8](repeating: 0, count: 4096)
        let n = recv(fd, &scratch, scratch.count, 0)
        if n > 0 { capture.save(Data(scratch.prefix(n))) }
        sendText(fd, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    }
    var encoded = URLRequest(url: URL(string: "http+unix://local/a%0D%0AX-Evil:%20yes?q=1")!)
    encoded.timeoutInterval = 1
    let (encodedTransport, encodedClient) = launchRequest(encoded, path: pathServer.path)
    try check(encodedClient.finished.wait(timeout: .now() + 1.5) == .success && encodedClient.error == nil,
              "percent-encoded request failed")
    try check(capture.read().starts(with: Data("GET /a%0D%0AX-Evil:%20yes?q=1 HTTP/1.1\r\n".utf8)),
              "percent-encoded path became raw request framing")
    withExtendedLifetime(encodedTransport) {}
    try pathServer.close()

    // These sizes include the terminal CRLFCRLF, not just joined header lines.
    let fixedWire = "GET /api/test HTTP/1.1\r\nHost: proletarius.local\r\nConnection: close\r\nX-Boundary: \r\n\r\n"
    let boundaryValue = String(repeating: "a", count: 64 * 1024 - fixedWire.utf8.count)
    let boundaryCapture = RequestCapture()
    let boundaryServer = try Server { fd in
        var wire = Data()
        var scratch = [UInt8](repeating: 0, count: 4096)
        let terminator = Data([13, 10, 13, 10])
        while wire.range(of: terminator) == nil {
            let n = recv(fd, &scratch, scratch.count, 0)
            if n <= 0 { break }
            wire.append(scratch, count: n)
        }
        boundaryCapture.save(wire)
        sendText(fd, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    }
    var boundaryRequest = URLRequest(url: URL(string: "http+unix://local/api/test")!)
    boundaryRequest.timeoutInterval = 1
    boundaryRequest.allHTTPHeaderFields = ["X-Boundary": boundaryValue]
    try check(boundaryRequest.allHTTPHeaderFields?["X-Boundary"] == boundaryValue,
              "Foundation changed boundary header")
    let (boundaryTransport, boundaryClient) = launchRequest(boundaryRequest, path: boundaryServer.path)
    try check(boundaryClient.finished.wait(timeout: .now() + 1.5) == .success && boundaryClient.error == nil,
              "65,536-byte request header rejected")
    try check(boundaryCapture.read().count == 64 * 1024, "accepted header wire length is not 65,536 bytes")
    withExtendedLifetime(boundaryTransport) {}
    try boundaryServer.close()
    try runRejected({ $0.allHTTPHeaderFields = ["X-Boundary": boundaryValue + "a"] }, expected: .badURL)

    let stalled = StalledInputStream()
    let streamServer = try Server { fd in readRequest(fd) }
    var streamRequest = URLRequest(url: URL(string: "http+unix://local/api/test")!)
    streamRequest.httpMethod = "POST"
    streamRequest.timeoutInterval = 0.2
    streamRequest.httpBodyStream = stalled
    let (streamTransport, streamClient) = launchRequest(streamRequest, path: streamServer.path)
    let streamCompleted = streamClient.finished.wait(timeout: .now() + 0.4) == .success
    stalled.release.signal()
    try check(streamCompleted && streamClient.error == .badURL,
              "stalled input stream was read or accepted: \(String(describing: streamClient.error))")
    try check(stalled.entered.wait(timeout: .now()) == .timedOut, "stalled input stream read was entered")
    withExtendedLifetime(streamTransport) {}
    try streamServer.close()

    try runRejected({ $0.httpMethod = "POST"; $0.httpBody = Data(repeating: 120, count: 64 * 1024 * 1024 + 1) },
                    expected: .dataLengthExceedsMaximum)
    print("PASS: request framing, exact 64 KiB wire header boundary, and bounded body/stream rejection")
}
private func responseTest(_ response: String, expected: String? = nil) throws {
    let server = try Server { fd in readRequest(fd); sendText(fd, response) }
    let (transport, client) = launch(path: server.path)
    defer { withExtendedLifetime(transport) {} }
    try check(client.finished.wait(timeout: .now() + 2) == .success, "response did not terminate")
    if let expected {
        try check(client.error == nil && client.body == Data(expected.utf8), "valid response rejected: \(String(describing: client.error))")
    } else {
        try check(client.error == .badServerResponse, "malformed response accepted: \(response.prefix(80))")
    }
    try server.close()
}
private func descriptorCount() -> Int { (0..<2048).reduce(0) { $0 + (fcntl(Int32($1), F_GETFD) >= 0 ? 1 : 0) } }

#if UNIX_SOCKET_TRANSPORT_TEST_HOOKS
private final class TransportGate: @unchecked Sendable {
    let reached = DispatchSemaphore(value: 0)
    let resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var storedFD: Int32 = -1
    private var storedFailure: String?
    var fd: Int32 { lock.lock(); defer { lock.unlock() }; return storedFD }
    var failure: String? { lock.lock(); defer { lock.unlock() }; return storedFailure }
    func recordFailure(_ message: String) {
        lock.lock(); storedFailure = storedFailure ?? message; lock.unlock()
    }
    func hold(_ fd: Int32) {
        lock.lock(); storedFD = fd; lock.unlock()
        reached.signal()
        if resume.wait(timeout: .now() + 3) != .success { recordFailure("test gate timed out") }
    }
}

private final class PendingConnectFacts: @unchecked Sendable {
    private let lock = NSLock()
    private var storedFD: Int32 = -1
    private var storedClosedFD: Int32 = -1
    private var storedCalls = 0
    private var storedChecks = 0
    private var storedPoll: Int32 = -1
    let gate = TransportGate()
    var fd: Int32 { lock.lock(); defer { lock.unlock() }; return storedFD }
    var closedFD: Int32 { lock.lock(); defer { lock.unlock() }; return storedClosedFD }
    var calls: Int { lock.lock(); defer { lock.unlock() }; return storedCalls }
    var pollResult: Int32 { lock.lock(); defer { lock.unlock() }; return storedPoll }
    func injected(_ fd: Int32) {
        lock.lock(); storedFD = fd; storedCalls += 1; lock.unlock()
    }
    func closed(_ fd: Int32) {
        lock.lock(); storedClosedFD = fd; lock.unlock()
    }
    func cancellationCheck(_ fd: Int32) {
        lock.lock(); storedChecks += 1; let count = storedChecks; lock.unlock()
        guard count == 2 else { return }
        // This closure is used only by completeConnect's real wait. Its first
        // check returned false and Darwin.poll has now actually returned. The
        // fixture peer never drains the send buffer: verify it is still full.
        var descriptor = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
        let result = Darwin.poll(&descriptor, 1, 0)
        lock.lock(); storedPoll = result; lock.unlock()
        gate.hold(fd)
    }
}

private func fixtureSocketPair() throws -> [Int32] {
    var pair: [Int32] = [-1, -1]
    try check(socketpair(AF_UNIX, SOCK_STREAM, 0, &pair) == 0, "fixture socketpair failed")
    var ready = false
    defer { if !ready { Darwin.close(pair[0]); Darwin.close(pair[1]) } }
    for fd in pair {
        var noSignal: Int32 = 1
        let flags = fcntl(fd, F_GETFL)
        try check(flags >= 0 && fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0,
                  "fixture nonblocking setup failed")
        try check(setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal,
                             socklen_t(MemoryLayout<Int32>.size)) == 0, "fixture SIGPIPE setup failed")
    }
    ready = true
    return pair
}

private func checkSocketPair(_ a: Int32, _ b: Int32, phase: String) throws {
    for (source, destination, value) in [(a, b, UInt8(65)), (b, a, UInt8(66))] {
        var byte = value
        try check(send(source, &byte, 1, 0) == 1, "\(phase): replacement write failed")
        var descriptor = pollfd(fd: destination, events: Int16(POLLIN), revents: 0)
        try check(Darwin.poll(&descriptor, 1, 1000) == 1, "\(phase): replacement read not ready")
        byte = 0
        try check(recv(destination, &byte, 1, 0) == 1 && byte == value,
                  "\(phase): replacement shutdown or closed")
    }
}

private func launchHooked(path: String, hooks: UnixSocketURLProtocol.TransportTestHooks)
    -> (UnixSocketURLProtocol, Client) {
    UnixSocketURLProtocol.socketPath = path
    var request = URLRequest(url: URL(string: "http+unix://local/api/test")!)
    request.timeoutInterval = 5
    let client = Client()
    let transport = UnixSocketURLProtocol(request: request, cachedResponse: nil, client: client)
    transport.installTransportTestHooks(hooks)
    transport.startLoading()
    return (transport, client)
}

private func forcedDescriptorReuseAfterRealClose() throws {
    // Allocate the replacement before the request so neither end is the old
    // worker descriptor. dup2 below, not chance allocation, forces equality.
    let pair = try fixtureSocketPair()
    var originalAliasOpen = true
    defer {
        if originalAliasOpen { Darwin.close(pair[0]) }
        Darwin.close(pair[1])
    }
    let server = try Server { fd in
        var buffer = [UInt8](repeating: 0, count: 4096)
        while recv(fd, &buffer, buffer.count, 0) > 0 {}
    }
    var serverClosed = false
    defer { if !serverClosed { try? server.close() } }
    let gate = TransportGate()
    let (transport, client) = launchHooked(path: server.path,
        hooks: .init(afterOwnedClose: { gate.hold($0) }))
    defer { transport.stopLoading(); gate.resume.signal(); transport.waitForTransportTestWorkerExit() }
    try check(server.accepted.wait(timeout: .now() + 1) == .success, "forced reuse request not connected")
    transport.stopLoading()
    try check(gate.reached.wait(timeout: .now() + 2) == .success, "real owned close not reached")
    let oldFD = gate.fd
    try check(oldFD >= 0 && oldFD != pair[0] && oldFD != pair[1], "replacement was not independent")
    try check(fcntl(oldFD, F_GETFD) == -1 && errno == EBADF, "worker did not really close old fd")
    try check(dup2(pair[0], oldFD) == oldFD, "dup2 did not force old descriptor number")
    defer { Darwin.close(oldFD) }
    try check(fcntl(oldFD, F_GETFD) >= 0, "replacement descriptor not open")
    Darwin.close(pair[0]); originalAliasOpen = false
    for _ in 0..<16 { transport.stopLoading() }
    try checkSocketPair(oldFD, pair[1], phase: "old worker held after close")
    gate.resume.signal()
    // Unlike a callback/notification just before return, this queue barrier
    // runs after the original perform closure has completely left the queue.
    transport.waitForTransportTestWorkerExit()
    try check(gate.failure == nil, "\(gate.failure ?? "gate failure")")
    for _ in 0..<16 { transport.stopLoading() }
    try check(fcntl(oldFD, F_GETFD) >= 0, "late cancellation closed reused fd after worker exit")
    try checkSocketPair(oldFD, pair[1], phase: "old worker fully exited")
    try check(client.callbacks == 0, "cancelled forced-reuse request delivered callback")
    try server.close()
    serverClosed = true
    print("PASS: dup2 forced same-fd reuse after actual close, late/repeated cancel before and after real worker exit")
}

private func pendingConnectCancellationWithRealPoll() throws {
    let pair = try fixtureSocketPair()
    defer { Darwin.close(pair[0]); Darwin.close(pair[1]) }
    var smallBuffer: Int32 = 4096
    try check(setsockopt(pair[0], SOL_SOCKET, SO_SNDBUF, &smallBuffer,
                         socklen_t(MemoryLayout<Int32>.size)) == 0, "fixture send buffer setup failed")
    let bytes = [UInt8](repeating: 120, count: 4096)
    var full = false
    for _ in 0..<4096 {
        let sent = bytes.withUnsafeBytes { send(pair[0], $0.baseAddress, $0.count, 0) }
        if sent < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) { full = true; break }
        try check(sent > 0, "fixture buffer fill failed")
    }
    try check(full, "fixture exceeded bounded16MiB fill without EAGAIN")
    var descriptor = pollfd(fd: pair[0], events: Int16(POLLOUT), revents: 0)
    try check(Darwin.poll(&descriptor, 1, 0) == 0, "pending fixture socket is writable")
    let facts = PendingConnectFacts()
    let (transport, client) = launchHooked(path: "/compile-only-connect-outcome-seam",
        hooks: .init(afterOwnedClose: { facts.closed($0) }, connectResult: { fd in
            facts.injected(fd)
            guard dup2(pair[0], fd) == fd else {
                facts.gate.recordFailure("pending fixture dup2 failed")
                return (-1, EBADF)
            }
            // Darwin did not naturally return EINPROGRESS on this platform's
            // backlog probe. Inject that outcome only, using a real full socket.
            return (-1, EINPROGRESS)
        }, pendingCancellationCheck: { facts.cancellationCheck($0) }))
    defer { transport.stopLoading(); facts.gate.resume.signal(); transport.waitForTransportTestWorkerExit() }
    try check(facts.gate.reached.wait(timeout: .now() + 2) == .success,
              "pending branch did not pass an actual poll iteration")
    try check(facts.calls == 1 && facts.pollResult == 0, "pending fixture lost nonwritable state")
    for _ in 0..<8 { transport.stopLoading() }
    facts.gate.resume.signal()
    transport.waitForTransportTestWorkerExit()
    try check(facts.gate.failure == nil, "\(facts.gate.failure ?? "gate failure")")
    try check(facts.closedFD == facts.fd && facts.fd >= 0, "pending cancellation missed owned close")
    try check(fcntl(facts.fd, F_GETFD) == -1 && errno == EBADF, "pending worker leaked owned descriptor")
    try check(client.callbacks == 0, "pending connect cancellation delivered callback")
    print("PASS: injected EINPROGRESS, real nonwritable fd/poll and production pending cancellation/close (not natural pending connect)")
}
#endif

@main enum TransportTests {
    static func main() throws {
        signal(SIGPIPE, SIG_DFL)
        try requestConstructionTests()
        try responseTest("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}", expected: "{}")
        try responseTest("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;extension=yes\r\n{}\r\n0\r\nX-Finished: yes\r\n\r\n", expected: "{}")
        try responseTest("HTTP/1.1 204 No Content\r\n\r\n", expected: "")
        let malformed = [
            "HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 67108865\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{",
            "garbage 200 OK\r\n\r\n",
            "HTTP/1.1 200 OK\r\nBroken Header\r\n\r\n",
            "HTTP/1.1 200 OK\r\nX: " + String(repeating: "x", count: 65536) + "\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFF\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4000001\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n-1\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}XX0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nBad trailer\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n" + String(repeating: "0", count: 8193) + "\r\n"
        ]
        for response in malformed { try responseTest(response) }
        print("PASS: valid fixed/chunked/no-body responses and 15 malformed framing/size boundaries")

        // Fragment all framing, including CRLF, across separate socket writes.
        let fragmented = try Server { fd in
            readRequest(fd)
            for byte in "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n".utf8 {
                _ = withUnsafeBytes(of: byte) { send(fd, $0.baseAddress, 1, 0) }; usleep(500)
            }
        }
        let (fragmentTransport, fragmentClient) = launch(path: fragmented.path)
        try check(fragmentClient.finished.wait(timeout: .now() + 2) == .success && fragmentClient.error == nil && fragmentClient.body == Data("{}".utf8), "fragmented response failed")
        withExtendedLifetime(fragmentTransport) {}
        try fragmented.close()
        print("PASS: fragmented response/chunk boundaries")

        for initial in ["", "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{", "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n"] {
            let server = try Server { fd in readRequest(fd); sendText(fd, initial); usleep(350_000) }
            let started = ProcessInfo.processInfo.systemUptime
            let (transport, client) = launch(path: server.path, timeout: 0.12)
            try check(client.finished.wait(timeout: .now() + 1) == .success && client.error == .timedOut, "stalled read did not time out")
            try check(ProcessInfo.processInfo.systemUptime - started < 0.3, "read timeout late")
            withExtendedLifetime(transport) {}
            try server.close()
        }
        let trickle = try Server { fd in
            readRequest(fd)
            for _ in 0..<8 { sendText(fd, "x"); usleep(40_000) }
        }
        let trickleStarted = ProcessInfo.processInfo.systemUptime
        let (trickleTransport, trickleClient) = launch(path: trickle.path, timeout: 0.12)
        try check(trickleClient.finished.wait(timeout: .now() + 1) == .success && trickleClient.error == .timedOut,
                  "continuous trickle reset the deadline")
        try check(ProcessInfo.processInfo.systemUptime - trickleStarted < 0.3, "absolute trickle deadline late")
        withExtendedLifetime(trickleTransport) {}
        try trickle.close()
        let blockedWriter = try Server { _ in usleep(350_000) }
        let (writeTransport, writeClient) = launch(path: blockedWriter.path, timeout: 0.12, body: Data(repeating: 120, count: 4 * 1024 * 1024))
        try check(writeClient.finished.wait(timeout: .now() + 1) == .success && writeClient.error == .timedOut, "blocked write did not time out")
        withExtendedLifetime(writeTransport) {}
        try blockedWriter.close()
        print("PASS: header/body/trailer stalls, trickle reads and blocked writes obey the absolute request deadline")

        // The process deliberately retains the default SIGPIPE disposition. An
        // AF_UNIX peer closing during a large send must report an error, not kill it.
        let closedPeer = try Server { _ in }
        let (closedTransport, closedClient) = launch(path: closedPeer.path, body: Data(repeating: 120, count: 4 * 1024 * 1024))
        try check(closedClient.finished.wait(timeout: .now() + 2) == .success && closedClient.error != nil, "peer closure did not report error")
        withExtendedLifetime(closedTransport) {}
        try closedPeer.close()
        print("PASS: peer closure during send survives with default SIGPIPE disposition")

        let refusedPath = "/tmp/switch-transport-missing-" + UUID().uuidString + ".sock"
        let (refusedTransport, refusedClient) = launch(path: refusedPath)
        try check(refusedClient.finished.wait(timeout: .now() + 1) == .success
                  && refusedClient.error == .cannotConnectToHost, "real AF_UNIX connect refusal lost classification")
        withExtendedLifetime(refusedTransport) {}
        print("PASS: real AF_UNIX missing-endpoint connection refusal")

#if UNIX_SOCKET_TRANSPORT_TEST_HOOKS
        let hookedBaseline = descriptorCount()
        try forcedDescriptorReuseAfterRealClose()
        try pendingConnectCancellationWithRealPoll()
        try check(descriptorCount() <= hookedBaseline, "compile-only fixtures leaked descriptors")
#endif

        let baseline = descriptorCount()
        for index in 0..<40 {
            let server = try Server { fd in
                var buffer = [UInt8](repeating: 0, count: 8192)
                while recv(fd, &buffer, buffer.count, 0) > 0 {}
            }
            let (transport, client) = launch(path: server.path, body: index % 2 == 0 ? Data(repeating: 120, count: 1024 * 1024) : nil)
            try check(server.accepted.wait(timeout: .now() + 1) == .success, "cancel fixture did not accept")
            for _ in 0..<4 { transport.stopLoading() }
            // Concurrent unrelated descriptor allocation is a smoke test; the
            // compile-only test above separately forces exact fd equality.
            var pair: [Int32] = [-1, -1]
            try check(socketpair(AF_UNIX, SOCK_STREAM, 0, &pair) == 0, "socketpair failed")
            sendText(pair[0], "x")
            var byte: UInt8 = 0
            try check(recv(pair[1], &byte, 1, 0) == 1 && byte == 120, "cancel damaged another descriptor")
            Darwin.close(pair[0]); Darwin.close(pair[1])
            try server.close()
            usleep(2_000)
            try check(client.callbacks == 0, "cancel delivered callbacks")
            withExtendedLifetime(transport) {}
        }
        usleep(30_000)
        try check(descriptorCount() <= baseline, "cancelled requests leaked descriptors")
        let neverStartedClient = Client()
        let neverStarted = UnixSocketURLProtocol(request: URLRequest(url: URL(string: "http+unix://local/")!), cachedResponse: nil, client: neverStartedClient)
        neverStarted.stopLoading(); neverStarted.startLoading(); usleep(20_000)
        try check(neverStartedClient.callbacks == 0, "cancel before start delivered callbacks")
        print("PASS: 40 repeated read/write cancellations, unrelated socket isolation, no fd leaks, cancellation before start")
        print("PASS: Unix socket transport stability regression suite (private local sockets only)")
    }
}
