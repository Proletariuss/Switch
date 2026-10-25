import Foundation
import Darwin

/// Test-only HTTP client for a real sandbox daemon. It performs no retries,
/// proxy lookup, network connection, or fabricated successful response.
enum UnixHandoff {
    enum Failure: Error, CustomStringConvertible {
        case environment(String)
        case unsafeSocketPath
        case invalidRequest
        case system(String, Int32)
        case timedOut
        case truncatedResponse
        case malformedResponse
        case unsupportedFraming
        case responseTooLarge

        var description: String {
            switch self {
            case .environment(let name): return "Unix handoff environment is invalid: \(name)"
            case .unsafeSocketPath: return "Unix handoff socket is outside the build root or unsafe"
            case .invalidRequest: return "Unix handoff request is invalid"
            case .system(let operation, let code): return "Unix handoff \(operation) failed (errno \(code))"
            case .timedOut: return "Unix handoff deadline exceeded"
            case .truncatedResponse: return "Unix handoff response ended before its complete HTTP frame"
            case .malformedResponse: return "Unix handoff HTTP response is malformed"
            case .unsupportedFraming: return "Unix handoff HTTP response framing is unsupported"
            case .responseTooLarge: return "Unix handoff response exceeds its size limit"
            }
        }
    }

    private static let bodyLimit = 8 * 1024 * 1024
    private static let headerLimit = 64 * 1024
    private static let wireLimit = bodyLimit + 2 * headerLimit
    private static let timeout: TimeInterval = 5

    /// Paths may have the production APIClient's omitted initial slash.
    /// HTTP non-2xx responses are returned intact for the caller to classify.
    static func request(method: String, path: String, body: Data? = nil) throws -> (Int, Data) {
        let environment = ProcessInfo.processInfo.environment
        guard let root = environment["SWITCH_BUILD_DIR"], !root.isEmpty else {
            throw Failure.environment("SWITCH_BUILD_DIR")
        }
        guard let suppliedPath = environment["SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET"],
              !suppliedPath.isEmpty else {
            throw Failure.environment("SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET")
        }
        guard let token = environment["SWITCH_FORWARDING_HANDOFF_LIVE_TOKEN"],
              !token.isEmpty, token.utf8.count <= 4096,
              token.utf8.allSatisfy({ (0x21...0x7e).contains($0) }) else {
            throw Failure.environment("SWITCH_FORWARDING_HANDOFF_LIVE_TOKEN")
        }
        let socketPath = try checkedSocketPath(suppliedPath, root: root)
        let target = path.hasPrefix("/") ? path : "/" + path
        guard ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"].contains(method),
              !path.isEmpty, !target.hasPrefix("//"), !target.contains("://"),
              !target.contains("#"), target.utf8.allSatisfy({ (0x21...0x7e).contains($0) }),
              (body?.count ?? 0) <= bodyLimit else {
            throw Failure.invalidRequest
        }
        var header = "\(method) \(target) HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
        header += "Accept: application/json\r\nx-proletarius-app-token: \(token)\r\n"
        if body != nil { header += "Content-Type: application/json\r\n" }
        header += "Content-Length: \(body?.count ?? 0)\r\n\r\n"
        var wire = Data(header.utf8)
        if let body { wire.append(body) }

        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw Failure.system("socket", errno) }
        defer { Darwin.close(descriptor) }
        let descriptorFlags = fcntl(descriptor, F_GETFD, 0)
        guard descriptorFlags >= 0,
              fcntl(descriptor, F_SETFD, descriptorFlags | FD_CLOEXEC) == 0 else {
            throw Failure.system("close-on-exec", errno)
        }
        let statusFlags = fcntl(descriptor, F_GETFL, 0)
        guard statusFlags >= 0,
              fcntl(descriptor, F_SETFL, statusFlags | O_NONBLOCK) == 0 else {
            throw Failure.system("nonblocking", errno)
        }
        var noSignal: Int32 = 1
        guard setsockopt(descriptor, SOL_SOCKET, SO_NOSIGPIPE, &noSignal,
                         socklen_t(MemoryLayout<Int32>.size)) == 0 else {
            throw Failure.system("no-sigpipe", errno)
        }
        var before = stat()
        guard lstat(socketPath, &before) == 0,
              before.st_mode & mode_t(S_IFMT) == mode_t(S_IFSOCK) else {
            throw Failure.unsafeSocketPath
        }
        try connect(descriptor, path: socketPath, deadline: deadline)
        var after = stat()
        guard lstat(socketPath, &after) == 0,
              after.st_mode & mode_t(S_IFMT) == mode_t(S_IFSOCK),
              before.st_dev == after.st_dev, before.st_ino == after.st_ino else {
            throw Failure.unsafeSocketPath
        }
        try writeAll(wire, to: descriptor, deadline: deadline)
        var reader = Reader(descriptor: descriptor, deadline: deadline)
        return try readResponse(&reader, method: method)
    }

    private static func checkedSocketPath(_ supplied: String, root: String) throws -> String {
        guard root.hasPrefix("/"), supplied.hasPrefix("/"),
              !root.utf8.contains(0), !supplied.utf8.contains(0) else {
            throw Failure.unsafeSocketPath
        }
        let rootURL = URL(fileURLWithPath: root, isDirectory: true)
            .standardizedFileURL.resolvingSymlinksInPath()
        guard rootURL.path != "/" else { throw Failure.unsafeSocketPath }
        var metadata = stat()
        guard lstat(rootURL.path, &metadata) == 0,
              metadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR) else {
            throw Failure.unsafeSocketPath
        }
        let socketURL = URL(fileURLWithPath: supplied).standardizedFileURL
        let parent = socketURL.deletingLastPathComponent().resolvingSymlinksInPath()
        let resolved = parent.appendingPathComponent(socketURL.lastPathComponent).path
        guard resolved.hasPrefix(rootURL.path + "/") else { throw Failure.unsafeSocketPath }
        // The final component is checked with lstat, so it cannot be a symlink.
        return resolved
    }

    private static func wait(_ descriptor: Int32, events: Int16, deadline: TimeInterval) throws {
        while true {
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            guard remaining > 0 else { throw Failure.timedOut }
            var item = pollfd(fd: descriptor, events: events, revents: 0)
            let milliseconds = Int32(max(1, min(5000, ceil(remaining * 1000))))
            let result = Darwin.poll(&item, 1, milliseconds)
            if result < 0 {
                if errno == EINTR { continue }
                throw Failure.system("poll", errno)
            }
            if result == 0 { throw Failure.timedOut }
            if item.revents & Int16(POLLNVAL) != 0 { throw Failure.system("poll", EBADF) }
            if item.revents & (events | Int16(POLLERR) | Int16(POLLHUP)) != 0 { return }
        }
    }

    private static func connect(_ descriptor: Int32, path: String, deadline: TimeInterval) throws {
        var address = sockaddr_un()
        let pathBytes = Array(path.utf8) + [UInt8(0)]
        guard pathBytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw Failure.unsafeSocketPath
        }
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutablePointer(to: &address.sun_path) { storage in
            pathBytes.withUnsafeBytes { bytes in
                UnsafeMutableRawPointer(storage).copyMemory(from: bytes.baseAddress!, byteCount: bytes.count)
            }
        }
        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if result == 0 { return }
        let connectError = errno
        guard connectError == EINPROGRESS || connectError == EINTR || connectError == EALREADY else {
            throw Failure.system("connect", connectError)
        }
        try wait(descriptor, events: Int16(POLLOUT), deadline: deadline)
        var socketError: Int32 = 0
        var size = socklen_t(MemoryLayout<Int32>.size)
        guard getsockopt(descriptor, SOL_SOCKET, SO_ERROR, &socketError, &size) == 0 else {
            throw Failure.system("connect-status", errno)
        }
        guard socketError == 0 else { throw Failure.system("connect", socketError) }
    }

    private static func writeAll(_ bytes: Data, to descriptor: Int32, deadline: TimeInterval) throws {
        var sent = 0
        while sent < bytes.count {
            try wait(descriptor, events: Int16(POLLOUT), deadline: deadline)
            let count = bytes.withUnsafeBytes { buffer in
                Darwin.send(descriptor, buffer.baseAddress!.advanced(by: sent), buffer.count - sent, 0)
            }
            if count < 0 {
                let code = errno
                if code == EINTR || code == EAGAIN || code == EWOULDBLOCK { continue }
                throw Failure.system("send", code)
            }
            guard count > 0 else { throw Failure.system("send", EPIPE) }
            sent += count
        }
    }

    private struct Reader {
        let descriptor: Int32
        let deadline: TimeInterval
        private var bytes: [UInt8] = []
        private var position = 0
        private var received = 0

        init(descriptor: Int32, deadline: TimeInterval) {
            self.descriptor = descriptor
            self.deadline = deadline
        }

        var hasBufferedBytes: Bool { position < bytes.count }

        mutating func readMore() throws {
            while true {
                try UnixHandoff.wait(descriptor, events: Int16(POLLIN), deadline: deadline)
                var block = [UInt8](repeating: 0, count: 65_536)
                let count = block.withUnsafeMutableBytes {
                    Darwin.recv(descriptor, $0.baseAddress!, $0.count, 0)
                }
                if count < 0 {
                    let code = errno
                    if code == EINTR || code == EAGAIN || code == EWOULDBLOCK { continue }
                    throw Failure.system("recv", code)
                }
                guard count > 0 else { throw Failure.truncatedResponse }
                guard count <= UnixHandoff.wireLimit - received else { throw Failure.responseTooLarge }
                received += count
                bytes.append(contentsOf: block.prefix(count))
                return
            }
        }

        mutating func exact(_ length: Int) throws -> Data {
            guard length >= 0, length <= UnixHandoff.bodyLimit else { throw Failure.responseTooLarge }
            while bytes.count - position < length { try readMore() }
            let value = Data(bytes[position..<(position + length)])
            position += length
            compact()
            return value
        }

        mutating func line(limit: Int = UnixHandoff.headerLimit) throws -> [UInt8] {
            var scan = position
            while true {
                while scan + 1 < bytes.count {
                    if bytes[scan] == 13 && bytes[scan + 1] == 10 {
                        guard scan - position <= limit else { throw Failure.responseTooLarge }
                        let value = Array(bytes[position..<scan])
                        position = scan + 2
                        compact()
                        return value
                    }
                    scan += 1
                }
                guard bytes.count - position <= limit + 1 else { throw Failure.responseTooLarge }
                try readMore()
            }
        }

        private mutating func compact() {
            if position >= 65_536 {
                bytes.removeFirst(position)
                position = 0
            }
        }
    }

    private static func headerField(_ bytes: [UInt8]) throws -> (String, String) {
        guard let colon = bytes.firstIndex(of: 58), colon > 0,
              bytes[..<colon].allSatisfy(isTokenByte),
              bytes[(colon + 1)...].allSatisfy({ $0 == 9 || (0x20...0x7e).contains($0) }) else {
            throw Failure.malformedResponse
        }
        let name = String(decoding: bytes[..<colon], as: UTF8.self).lowercased()
        let value = String(decoding: bytes[(colon + 1)...], as: UTF8.self)
            .trimmingCharacters(in: CharacterSet(charactersIn: " \t"))
        return (name, value)
    }

    private static func isTokenByte(_ byte: UInt8) -> Bool {
        (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
            || Array("!#$%&'*+-.^_`|~".utf8).contains(byte)
    }

    private static func readResponse(_ reader: inout Reader, method: String) throws -> (Int, Data) {
        let first = try reader.line()
        guard first.allSatisfy({ (0x20...0x7e).contains($0) }) else { throw Failure.malformedResponse }
        let parts = String(decoding: first, as: UTF8.self).split(separator: " ", maxSplits: 2)
        guard parts.count >= 2, ["HTTP/1.1", "HTTP/1.0"].contains(String(parts[0])),
              parts[1].utf8.count == 3, parts[1].utf8.allSatisfy({ (48...57).contains($0) }),
              let status = Int(parts[1]), (200...599).contains(status) else {
            throw Failure.malformedResponse
        }
        var headers: [String: [String]] = [:]
        var headerBytes = first.count + 2
        while true {
            let line = try reader.line()
            headerBytes += line.count + 2
            guard headerBytes <= headerLimit else { throw Failure.responseTooLarge }
            if line.isEmpty { break }
            let (key, value) = try headerField(line)
            headers[key, default: []].append(value)
        }
        let lengths = headers["content-length"] ?? []
        let encodings = headers["transfer-encoding"] ?? []
        guard lengths.count <= 1, encodings.count <= 1,
              lengths.isEmpty || encodings.isEmpty else { throw Failure.malformedResponse }
        if let encoding = headers["content-encoding"],
           encoding.count != 1 || encoding[0].lowercased() != "identity" {
            throw Failure.unsupportedFraming
        }
        if method == "HEAD" || status == 204 || status == 304 {
            guard encodings.isEmpty, !reader.hasBufferedBytes else { throw Failure.malformedResponse }
            return (status, Data())
        }
        if let encoding = encodings.first {
            guard encoding.lowercased() == "chunked" else { throw Failure.unsupportedFraming }
            var body = Data()
            while true {
                let line = try reader.line()
                guard line.allSatisfy({ $0 == 9 || (0x20...0x7e).contains($0) }) else {
                    throw Failure.malformedResponse
                }
                let sizeBytes = line.prefix(while: { $0 != 59 })
                guard !sizeBytes.isEmpty,
                      sizeBytes.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
                      let count = UInt64(String(decoding: sizeBytes, as: UTF8.self), radix: 16) else {
                    throw Failure.malformedResponse
                }
                guard count <= UInt64(bodyLimit - body.count) else { throw Failure.responseTooLarge }
                if count == 0 {
                    var trailerBytes = 0
                    while true {
                        let trailer = try reader.line()
                        trailerBytes += trailer.count + 2
                        guard trailerBytes <= headerLimit else { throw Failure.responseTooLarge }
                        if trailer.isEmpty { break }
                        let (key, _) = try headerField(trailer)
                        guard key != "content-length", key != "transfer-encoding" else {
                            throw Failure.malformedResponse
                        }
                    }
                    guard !reader.hasBufferedBytes else { throw Failure.malformedResponse }
                    return (status, body)
                }
                body.append(try reader.exact(Int(count)))
                guard try reader.exact(2) == Data([13, 10]) else { throw Failure.malformedResponse }
            }
        }
        guard let length = lengths.first else { throw Failure.unsupportedFraming }
        guard !length.isEmpty, length.utf8.allSatisfy({ (48...57).contains($0) }),
              let count = UInt64(length) else { throw Failure.malformedResponse }
        guard count <= UInt64(bodyLimit) else { throw Failure.responseTooLarge }
        let body = try reader.exact(Int(count))
        guard !reader.hasBufferedBytes else { throw Failure.malformedResponse }
        return (status, body)
    }
}
