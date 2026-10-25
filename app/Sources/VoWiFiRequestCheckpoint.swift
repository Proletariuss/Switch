import Foundation
import Darwin

struct VoWiFiRequestCheckpoint: Codable, Equatable {
    let clientRequestID: String
    let requestedEnable: Bool
    /// Exact original JSON bytes, including owner/generation and raw user facts.
    let payload: Data
}

/// App-data journal, independent of GUI/daemon lifetime. An unreadable journal
/// fails closed; only a matching explicit Rust terminal receipt removes an ID.
final class VoWiFiRequestCheckpointStore {
    private let path: URL

    init(scope: String, root: URL? = nil) {
        let environment = ProcessInfo.processInfo.environment
        let base = root ?? environment["SWITCH_DATA_DIR"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("Switch", isDirectory: true)
        path = base.appendingPathComponent("vowifi-\(scope)-gui-requests.json")
    }

    func all() throws -> [VoWiFiRequestCheckpoint] {
        guard FileManager.default.fileExists(atPath: path.path) else { return [] }
        return try JSONDecoder().decode([VoWiFiRequestCheckpoint].self, from: Data(contentsOf: path))
    }

    func load() throws -> VoWiFiRequestCheckpoint? { try all().last }

    func begin<Request: Encodable>(id: String, enable: Bool, request: Request) throws -> VoWiFiRequestCheckpoint {
        var records = try all()
        let record = VoWiFiRequestCheckpoint(
            clientRequestID: id, requestedEnable: enable, payload: try JSONEncoder().encode(request))
        if let existing = records.first(where: { $0.clientRequestID == id }) {
            guard existing == record else { throw CocoaError(.fileWriteUnknown) }
            return existing
        }
        records.append(record)
        try write(records)
        return record
    }

    func clear(terminal receipt: VoWiFiOperation) throws {
        guard receipt.terminal, let id = receipt.clientRequestID else { throw CocoaError(.fileWriteUnknown) }
        var records = try all()
        guard let index = records.firstIndex(where: {
            $0.clientRequestID == id && $0.requestedEnable == receipt.requestedEnable
        }) else { return }
        records.remove(at: index)
        try write(records)
    }

    private func write(_ records: [VoWiFiRequestCheckpoint]) throws {
        let directory = path.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        try JSONEncoder().encode(records).write(to: path, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path.path)
        let file = try FileHandle(forWritingTo: path)
        try file.synchronize()
        try file.close()
        let descriptor = open(directory.path, O_RDONLY)
        guard descriptor >= 0 else { throw CocoaError(.fileWriteUnknown) }
        defer { close(descriptor) }
        guard fsync(descriptor) == 0 else { throw CocoaError(.fileWriteUnknown) }
    }
}
