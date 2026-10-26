import Darwin
import Foundation

/// Best-effort diagnostics must never stall BLE authentication or keepalive.
/// A slot covers both queued and executing work; full queues drop the event.
final class BoundedBLEDiagnosticLog: @unchecked Sendable {
    static let shared = BoundedBLEDiagnosticLog(directory: { SwitchRuntimePaths.dataDirectory() })
    static let capacity = 32
    static let maximumEventBytes = 2 * 1024
    static let maximumFileBytes = 64 * 1024
    static let fileNames = ["ble_diag.log", "ble_diag.log.1", "ble_diag.log.2"]
    private static let lockName = ".ble_diag.lock"
    private enum Failure: Error { case unsafePath, unavailable }

    private let writer = DispatchQueue(label: "com.proletarius.ble-diagnostics", qos: .utility)
    private let slots = DispatchSemaphore(value: capacity)
    private let directory: () -> URL
    private let fileByteLimit: Int
    private let names: [String]
    private let onWriteFailure: (@Sendable () -> Void)?
    // A device-free sink seam; it replaces only the final IO effect. Admission,
    // queue ownership, timestamp/UTF-8 bounds and slot release stay production.
    private let sink: ((Data) throws -> Void)?
    private lazy var formatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()
    private lazy var destination = directory()

    init(directory: @escaping () -> URL, sink: ((Data) throws -> Void)? = nil,
         maximumFileBytes: Int = BoundedBLEDiagnosticLog.maximumFileBytes,
         fileNames: [String] = BoundedBLEDiagnosticLog.fileNames,
         onWriteFailure: (@Sendable () -> Void)? = nil) {
        precondition(maximumFileBytes >= Self.maximumEventBytes && maximumFileBytes <= 2 * 1024 * 1024)
        precondition(fileNames.count == 3 && Set(fileNames).count == 3
            && fileNames.allSatisfy { !$0.isEmpty && !$0.contains("/") && !$0.contains("..") })
        self.directory = directory
        self.sink = sink
        self.fileByteLimit = maximumFileBytes
        self.names = fileNames
        self.onWriteFailure = onWriteFailure
    }

    @discardableResult
    func enqueue(_ message: String) -> Bool {
        guard slots.wait(timeout: .now()) == .success else { return false }
        // UTC timestamp + space + LF occupies 22 bytes. Copy at most one byte
        // beyond the allowance, rather than encoding an unbounded input first.
        let bytes = Data(message.utf8.prefix(Self.maximumEventBytes - 22 + 1))
        guard bytes.count <= Self.maximumEventBytes - 22 else {
            slots.signal()
            return false
        }
        writer.async { [self] in
            defer { slots.signal() }
            var event = Data((formatter.string(from: Date()) + " ").utf8)
            event.append(bytes)
            event.append(0x0a)
            guard event.count <= Self.maximumEventBytes else { return }
            do {
                if let sink { try sink(event) }
                else { try append(event) }
            } catch {
                onWriteFailure?()
                // Disk, permissions, lock contention and rotation failures
                // discard only diagnostics. Never retry on a BLE callback.
            }
        }
        return true
    }

    static func isPrivateDirectory(_ metadata: stat, user: uid_t = geteuid()) -> Bool {
        metadata.st_uid == user && metadata.st_mode & S_IFMT == S_IFDIR
            && metadata.st_mode & 0o777 == 0o700
    }

    static func isPrivateRegularFile(_ metadata: stat, user: uid_t = geteuid()) -> Bool {
        metadata.st_mode & S_IFMT == S_IFREG && metadata.st_uid == user
            && metadata.st_mode & 0o777 == 0o600 && metadata.st_nlink == 1
    }

    private func openDirectory() throws -> Int32 {
        guard destination.isFileURL, destination.path.hasPrefix("/") else { throw Failure.unsafePath }
        let components = destination.standardizedFileURL.path.split(separator: "/")
        guard !components.isEmpty else { throw Failure.unsafePath }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard descriptor >= 0 else { throw Failure.unavailable }
        do {
            for component in components {
                var next = openat(descriptor, String(component), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                if next < 0, errno == ENOENT {
                    guard mkdirat(descriptor, String(component), 0o700) == 0 || errno == EEXIST else {
                        throw Failure.unavailable
                    }
                    next = openat(descriptor, String(component), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                }
                guard next >= 0 else { throw Failure.unsafePath }
                close(descriptor)
                descriptor = next
            }
            var metadata = stat()
            guard fstat(descriptor, &metadata) == 0, Self.isPrivateDirectory(metadata) else {
                throw Failure.unsafePath
            }
            return descriptor
        } catch {
            close(descriptor)
            throw error
        }
    }

    private func safeFile(_ directory: Int32, _ name: String) throws -> stat? {
        var metadata = stat()
        if fstatat(directory, name, &metadata, AT_SYMLINK_NOFOLLOW) != 0 {
            guard errno == ENOENT else { throw Failure.unavailable }
            return nil
        }
        guard Self.isPrivateRegularFile(metadata) else {
            throw Failure.unsafePath
        }
        return metadata
    }

    private func matches(_ directory: Int32, _ name: String, _ expected: stat?) throws -> Bool {
        let current = try safeFile(directory, name)
        switch (current, expected) {
        case (nil, nil): return true
        case let (current?, expected?): return current.st_dev == expected.st_dev && current.st_ino == expected.st_ino
        default: return false
        }
    }

    private func append(_ event: Data) throws {
        let directory = try openDirectory()
        defer { close(directory) }
        // Nonblocking OS ownership also bounds rotation across GUI processes.
        // Do not follow, repair or replace an existing unsafe lock item.
        let lock = openat(directory, Self.lockName, O_RDWR | O_CREAT | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC, 0o600)
        guard lock >= 0 else { throw Failure.unsafePath }
        defer { close(lock) }
        var lockInfo = stat()
        guard fstat(lock, &lockInfo) == 0, Self.isPrivateRegularFile(lockInfo),
              try matches(directory, Self.lockName, lockInfo), flock(lock, LOCK_EX | LOCK_NB) == 0 else {
            throw Failure.unsafePath
        }
        defer { _ = flock(lock, LOCK_UN) }
        var files = try names.map { try safeFile(directory, $0) }
        guard files.allSatisfy({ $0 == nil || ($0!.st_size >= 0 && $0!.st_size <= fileByteLimit) }) else {
            // Preserve pre-existing oversized/unsafe files for explicit repair.
            throw Failure.unsafePath
        }
        if let current = files[0], current.st_size + Int64(event.count) > fileByteLimit {
            if files[2] != nil {
                guard try matches(directory, names[2], files[2]),
                      unlinkat(directory, names[2], 0) == 0 else { throw Failure.unavailable }
            }
            if files[1] != nil {
                guard try matches(directory, names[1], files[1]),
                      try matches(directory, names[2], nil),
                      renameat(directory, names[1], directory, names[2]) == 0 else {
                    throw Failure.unavailable
                }
            }
            guard try matches(directory, names[0], files[0]),
                  try matches(directory, names[1], nil),
                  renameat(directory, names[0], directory, names[1]) == 0 else {
                throw Failure.unavailable
            }
            files[0] = nil
        }
        let flags = O_WRONLY | O_APPEND | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC
            | (files[0] == nil ? O_CREAT | O_EXCL : 0)
        let descriptor = openat(directory, names[0], flags, 0o600)
        guard descriptor >= 0 else { throw Failure.unsafePath }
        defer { close(descriptor) }
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0, Self.isPrivateRegularFile(metadata),
              try matches(directory, names[0], metadata),
              metadata.st_size >= 0, metadata.st_size <= fileByteLimit,
              metadata.st_size + Int64(event.count) <= fileByteLimit else {
            throw Failure.unsafePath
        }
        if let expected = files[0] {
            guard expected.st_dev == metadata.st_dev, expected.st_ino == metadata.st_ino else { throw Failure.unsafePath }
        }
        try event.withUnsafeBytes { bytes in
            var offset = 0
            while offset < bytes.count {
                let count = Darwin.write(descriptor, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                if count < 0, errno == EINTR { continue }
                guard count > 0 else { throw Failure.unavailable }
                offset += count
            }
        }
    }


    /// Call only on a background reader. No flush, retry, or writer join: a
    /// partial newest line is discarded by the typed export decoder.
    func snapshotSegments() throws -> [Data] {
        let directory = try openDirectory()
        defer { close(directory) }
        var result: [Data] = []
        for name in names.reversed() {
            guard let expected = try safeFile(directory, name) else { continue }
            guard expected.st_size >= 0, expected.st_size <= fileByteLimit else { throw Failure.unsafePath }
            let fd = openat(directory, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
            guard fd >= 0 else { throw Failure.unavailable }
            defer { close(fd) }
            var actual = stat()
            guard fstat(fd, &actual) == 0, Self.isPrivateRegularFile(actual),
                  actual.st_dev == expected.st_dev, actual.st_ino == expected.st_ino else { throw Failure.unsafePath }
            var data = Data()
            var buffer = [UInt8](repeating: 0, count: 8192)
            while data.count <= fileByteLimit {
                let count = Darwin.read(fd, &buffer, min(buffer.count, fileByteLimit + 1 - data.count))
                if count < 0 && errno == EINTR { continue }
                guard count >= 0 else { throw Failure.unavailable }
                if count == 0 { break }
                data.append(contentsOf: buffer.prefix(count))
            }
            guard data.count <= fileByteLimit else { throw Failure.unsafePath }
            result.append(data)
        }
        return result
    }

    #if SWITCH_BLE_DIAGNOSTIC_TESTING
    func waitUntilIdleForTesting() { writer.sync {} }
    #endif
}
