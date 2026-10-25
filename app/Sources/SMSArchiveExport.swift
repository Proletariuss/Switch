import Foundation
import Darwin

struct SMSExportProfileOption: Identifiable, Sendable { let id: String; let title: String }
struct SMSExportSubscriptionOption: Identifiable, Sendable { let id: String; let title: String; let device: String }

struct SMSArchiveExportRequest: Encodable, Sendable {
    var source = "all"
    var privacy = "masked"
    var device: String?
    var profileID: String?
    var subscriptionID: String?
    var peer: String?
    var fromMS: UInt64?
    var toMS: UInt64?
    enum CodingKeys: String, CodingKey {
        case source, privacy, device, peer
        case profileID = "profile_id", subscriptionID = "subscription_id", fromMS = "from_ms", toMS = "to_ms"
    }
}
struct SMSArchiveExportMetadata: Decodable, Sendable {
    let schemaVersion: Int
    let token: String
    let totalBytes: UInt64
    let messageCount: UInt64
    let chunkBytes: Int
    let expiresAtMS: UInt64
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", token, totalBytes = "total_bytes", messageCount = "message_count"
        case chunkBytes = "chunk_bytes", expiresAtMS = "expires_at_ms"
    }
}
struct SMSArchiveExportSummary: Sendable { let messageCount: UInt64; let totalBytes: UInt64 }

protocol SMSArchiveExportAPI: Sendable {
    func prepare(_ request: SMSArchiveExportRequest) async throws -> SMSArchiveExportMetadata
    func chunk(token: String, offset: UInt64) async throws -> Data
    func remove(token: String) async throws
}
actor NativeSMSArchiveExportAPI: SMSArchiveExportAPI {
    private let client = APIClient(timeoutInterval: 45)
    func prepare(_ request: SMSArchiveExportRequest) async throws -> SMSArchiveExportMetadata {
        try await client.send("api/v2/sms-export", body: request)
    }
    func chunk(token: String, offset: UInt64) async throws -> Data {
        try await client.getData("api/v2/sms-export/\(token)?offset=\(offset)")
    }
    func remove(token: String) async throws {
        try await APIClient(timeoutInterval: 10).send("api/v2/sms-export/\(token)", method: "DELETE")
    }
}

enum SMSArchiveExportError: LocalizedError {
    case busy, invalidMetadata, invalidChunk, invalidDestination, fileFailure, commitUnconfirmed
    var errorDescription: String? {
        switch self {
        case .busy: return "已有短信导出正在进行。"
        case .invalidMetadata: return "导出信息不完整，请重新导出。"
        case .invalidChunk: return "导出数据不完整，目标文件未替换。"
        case .invalidDestination: return "请选择普通 JSON 文件作为保存位置。"
        case .fileFailure: return "无法保存短信导出，请检查磁盘空间与文件权限。"
        case .commitUnconfirmed: return "文件已写入，但目录同步未确认，请检查目标文件。"
        }
    }
}

/// An independent actor keeps filesystem writes off the UI actor. Only a single
/// bounded chunk is retained; SMS stores/cursors are never read or mutated here.
actor SMSArchiveExporter {
    static let chunkLimit = 256 * 1024
    static let outputLimit: UInt64 = 128 * 1024 * 1024
    private let api: any SMSArchiveExportAPI
    private var active = false
    init(api: any SMSArchiveExportAPI = NativeSMSArchiveExportAPI()) { self.api = api }

    func save(_ request: SMSArchiveExportRequest, to destination: URL,
              progress: @escaping @MainActor @Sendable (UInt64, UInt64) -> Void = { _, _ in }) async throws -> SMSArchiveExportSummary {
        guard !active else { throw SMSArchiveExportError.busy }
        active = true
        defer { active = false }
        try Task.checkCancellation()
        let output = try SMSExportAtomicFile(destination: destination)
        defer { output.discardTemporary() }
        var token: String?
        do {
            let metadata = try await api.prepare(request)
            // Only a validated opaque token may be interpolated into a URL.
            if Self.validToken(metadata.token) { token = metadata.token }
            guard token != nil, metadata.schemaVersion == 1,
                  metadata.totalBytes > 0, metadata.totalBytes <= Self.outputLimit,
                  metadata.chunkBytes == Self.chunkLimit, metadata.expiresAtMS > 0 else {
                throw SMSArchiveExportError.invalidMetadata
            }
            try Task.checkCancellation()
            var offset: UInt64 = 0
            await progress(0, metadata.totalBytes)
            while offset < metadata.totalBytes {
                try Task.checkCancellation()
                let data = try await api.chunk(token: metadata.token, offset: offset)
                try Task.checkCancellation()
                let expected = min(UInt64(Self.chunkLimit), metadata.totalBytes - offset)
                guard UInt64(data.count) == expected else { throw SMSArchiveExportError.invalidChunk }
                try output.append(data)
                offset += UInt64(data.count)
                await progress(offset, metadata.totalBytes)
            }
            // The server's total and exact chunk boundaries must agree, including
            // an explicit EOF; a changing or truncated export is never installed.
            let tail = try await api.chunk(token: metadata.token, offset: offset)
            guard tail.isEmpty else { throw SMSArchiveExportError.invalidChunk }
            try Task.checkCancellation()
            try output.commit()
            await cleanup(metadata.token)
            return SMSArchiveExportSummary(messageCount: metadata.messageCount, totalBytes: metadata.totalBytes)
        } catch {
            output.discardTemporary()
            if let token { await cleanup(token) }
            throw error
        }
    }
    private func cleanup(_ token: String) async {
        let api = self.api
        // A separate task does not inherit cancellation. If this best-effort
        // DELETE cannot reach the backend, its ten-minute TTL still closes it.
        await Task.detached { try? await api.remove(token: token) }.value
    }
    private static func validToken(_ token: String) -> Bool {
        (32...64).contains(token.utf8.count) && token.utf8.allSatisfy {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95
        }
    }
}

/// The temporary file lives next to the selected destination, so rename is one
/// filesystem operation. No secret body is written before 0600 is established.
private final class SMSExportAtomicFile {
    private let destination: URL
    private let temporary: URL
    private var handle: FileHandle?
    private var committed = false
    init(destination: URL) throws {
        guard destination.isFileURL, !destination.lastPathComponent.isEmpty,
              destination.lastPathComponent != ".", destination.lastPathComponent != ".." else {
            throw SMSArchiveExportError.invalidDestination
        }
        self.destination = destination
        temporary = destination.deletingLastPathComponent().appendingPathComponent(".switch-sms-export-\(UUID().uuidString).tmp")
        try Self.validateTarget(destination)
        let fd = temporary.path.withCString { Darwin.open($0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, S_IRUSR | S_IWUSR) }
        guard fd >= 0 else { throw SMSArchiveExportError.fileFailure }
        guard fchmod(fd, S_IRUSR | S_IWUSR) == 0 else {
            Darwin.close(fd); try? FileManager.default.removeItem(at: temporary)
            throw SMSArchiveExportError.fileFailure
        }
        handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
    }
    func append(_ data: Data) throws {
        guard let handle else { throw SMSArchiveExportError.fileFailure }
        do { try handle.write(contentsOf: data) } catch { throw SMSArchiveExportError.fileFailure }
    }
    func commit() throws {
        guard let handle else { throw SMSArchiveExportError.fileFailure }
        do { try handle.synchronize(); try handle.close() } catch { throw SMSArchiveExportError.fileFailure }
        self.handle = nil
        try Self.validateTarget(destination)
        let result = temporary.path.withCString { from in destination.path.withCString { to in Darwin.rename(from, to) } }
        guard result == 0 else { throw SMSArchiveExportError.fileFailure }
        committed = true
        let directory = destination.deletingLastPathComponent().path.withCString { Darwin.open($0, O_RDONLY | O_DIRECTORY | O_CLOEXEC) }
        guard directory >= 0 else { throw SMSArchiveExportError.commitUnconfirmed }
        let synced = fsync(directory) == 0
        let closed = Darwin.close(directory) == 0
        guard synced && closed else { throw SMSArchiveExportError.commitUnconfirmed }
    }
    func discardTemporary() {
        try? handle?.close(); handle = nil
        if !committed { try? FileManager.default.removeItem(at: temporary) }
    }
    private static func validateTarget(_ url: URL) throws {
        var info = stat()
        let result = url.path.withCString { lstat($0, &info) }
        if result == 0 {
            guard info.st_mode & S_IFMT == S_IFREG else { throw SMSArchiveExportError.invalidDestination }
        } else if errno != ENOENT { throw SMSArchiveExportError.fileFailure }
    }
}
