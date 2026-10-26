import Foundation
import Darwin

struct DeviceSMSAuthorityStatus: Codable, Equatable {
    let identityVersion: Int
    let family: String
    let authorityID: String
    let epoch: String
    let issuedHighWater: String
    let nextSequence: String?
    let nextClientRequestID: String?
    enum CodingKeys: String, CodingKey {
        case family, epoch
        case identityVersion = "identity_version", authorityID = "authority_id"
        case issuedHighWater = "issued_high_water", nextSequence = "next_sequence"
        case nextClientRequestID = "next_client_request_id"
    }
    func candidate() throws -> (String, String) {
        guard identityVersion == 2, family == "device_sms", Self.hex32(authorityID),
              let epochValue = Self.counter(epoch), epochValue > 0,
              let high = Self.counter(issuedHighWater), high < UInt64.max,
              let nextSequence, let sequence = Self.counter(nextSequence), sequence == high + 1,
              let nextClientRequestID,
              nextClientRequestID == Self.identifier(authority: authorityID, epoch: epochValue, sequence: sequence)
        else { throw DeviceSMSJournalError.invalid }
        return (nextSequence, nextClientRequestID)
    }
    static func counter(_ value: String) -> UInt64? {
        guard let result = UInt64(value), String(result) == value else { return nil }
        return result
    }
    static func hex32(_ value: String) -> Bool {
        value.utf8.count == 32 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func identifier(authority: String, epoch: UInt64, sequence: UInt64) -> String {
        "m2:ds:" + authority + ":" + String(format: "%016llx", epoch) + ":" + String(format: "%016llx", sequence)
    }
}

struct DeviceSMSReserveRequest: Codable, Equatable {
    struct Payload: Codable, Equatable {
        let phone: String
        let message: String
        let expectedSubscriptionID: String
        enum CodingKeys: String, CodingKey {
            case phone, message
            case expectedSubscriptionID = "expected_subscription_id"
        }
    }
    let identityVersion: Int
    let family: String
    let authorityID: String
    let epoch: String
    let sequence: String
    let clientRequestID: String
    let intentNonce: String
    let purpose: String
    let device: String
    let payload: Payload
    enum CodingKeys: String, CodingKey {
        case family, epoch, sequence, purpose, device, payload
        case identityVersion = "identity_version", authorityID = "authority_id"
        case clientRequestID = "client_request_id", intentNonce = "intent_nonce"
    }
    var request: DeviceSMSIntentRequest {
        DeviceSMSIntentRequest(clientRequestID: clientRequestID, phone: payload.phone,
            message: payload.message, device: device,
            expectedSubscriptionID: payload.expectedSubscriptionID, identityVersion: 2, intentNonce: intentNonce)
    }
    func validate() throws {
        guard identityVersion == 2, family == "device_sms", purpose == "device_sms_send",
              DeviceSMSAuthorityStatus.hex32(authorityID), DeviceSMSAuthorityStatus.hex32(intentNonce),
              let e = DeviceSMSAuthorityStatus.counter(epoch), e > 0,
              let s = DeviceSMSAuthorityStatus.counter(sequence), s > 0,
              clientRequestID == DeviceSMSAuthorityStatus.identifier(authority: authorityID, epoch: e, sequence: s),
              ["reader", "dji"].contains(device), !payload.phone.isEmpty,
              payload.phone == payload.phone.trimmingCharacters(in: .whitespacesAndNewlines),
              !payload.message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              payload.phone.utf8.count <= 256, payload.message.utf8.count <= 65_536,
              payload.expectedSubscriptionID.utf8.count <= 256
        else { throw DeviceSMSJournalError.invalid }
    }
    static func candidate(status: DeviceSMSAuthorityStatus, phone: String, message: String,
                          device: String, subscription: String?) throws -> Self {
        let (sequence, id) = try status.candidate()
        let value = Self(identityVersion: 2, family: "device_sms", authorityID: status.authorityID,
            epoch: status.epoch, sequence: sequence, clientRequestID: id,
            intentNonce: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(),
            purpose: "device_sms_send", device: device,
            payload: Payload(phone: phone, message: message, expectedSubscriptionID: subscription ?? ""))
        try value.validate()
        return value
    }
    /// Only cancellation can use this projection. It never creates a send checkpoint.
    static func cancellation(_ record: DeviceSMSIntent) throws -> Self {
        let parts = record.id.split(separator: ":", omittingEmptySubsequences: false)
        guard record.version == 2, record.state == .reserved, parts.count == 5,
              parts[0] == "m2", parts[1] == "ds", parts[3].count == 16, parts[4].count == 16,
              let epoch = UInt64(parts[3], radix: 16), let sequence = UInt64(parts[4], radix: 16),
              let nonce = record.intentNonce, let subscription = record.expectedSubscriptionID
        else { throw DeviceSMSJournalError.invalid }
        let value = Self(identityVersion: 2, family: "device_sms", authorityID: String(parts[2]),
            epoch: String(epoch), sequence: String(sequence), clientRequestID: record.id,
            intentNonce: nonce, purpose: "device_sms_send", device: record.device,
            payload: Payload(phone: record.phone, message: record.message, expectedSubscriptionID: subscription))
        try value.validate()
        return value
    }
}

enum DeviceSMSJournalError: Error { case invalid, unavailable, conflict, capacity }

struct DeviceSMSIssuanceCheckpoint: Codable, Equatable {
    enum Phase: String, Codable { case candidate, reserved, prepared, dispatched, unresolved }
    let reserveBytes: Data
    let sendBytes: Data
    let createdAtMS: Int64
    var phase: Phase
    var reserve: DeviceSMSReserveRequest { get throws { try JSONDecoder().decode(DeviceSMSReserveRequest.self, from: reserveBytes) } }
    var request: DeviceSMSIntentRequest { get throws { try JSONDecoder().decode(DeviceSMSIntentRequest.self, from: sendBytes) } }
    static func make(_ reserve: DeviceSMSReserveRequest) throws -> Self {
        try reserve.validate()
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return Self(reserveBytes: try encoder.encode(reserve), sendBytes: try encoder.encode(reserve.request),
                    createdAtMS: Int64(Date().timeIntervalSince1970 * 1000), phase: .candidate)
    }
    func validate() throws {
        guard reserveBytes.count <= 131_072, sendBytes.count <= 131_072, createdAtMS > 0 else { throw DeviceSMSJournalError.invalid }
        let reservation = try reserve
        try reservation.validate()
        guard try request == reservation.request else { throw DeviceSMSJournalError.invalid }
    }
    func sameBytes(_ other: Self) -> Bool { reserveBytes == other.reserveBytes && sendBytes == other.sendBytes }
    func samePayload(phone: String, message: String, device: String, subscription: String?) throws -> Bool {
        let value = try request
        return value.phone == phone && value.message == message && value.device == device
            && (value.expectedSubscriptionID ?? "") == (subscription ?? "")
    }
    func projection() throws -> DeviceSMSIntent {
        let value = try request
        let reservation = try reserve
        let state: DeviceSMSSendState = [.candidate, .reserved].contains(phase) ? .reserved : (phase == .prepared ? .prepared : .unknown)
        return DeviceSMSIntent(clientRequestID: value.clientRequestID, phone: value.phone, message: value.message,
            device: value.device, subscriptionID: value.expectedSubscriptionID, state: state, segments: 0,
            archiveConfirmed: false, canSubmit: phase == .prepared, createdAtMS: createdAtMS, updatedAtMS: createdAtMS,
            identityVersion: 2, intentNonce: reservation.intentNonce, expectedSubscriptionID: value.expectedSubscriptionID)
    }
}

/// The stable lock file is never replaced or removed. Every snapshot operation
/// is under flock; replacement uses a private same-directory file plus file and
/// directory fsync. The exact bytes are read back before any caller may POST.
final class DeviceSMSIssuanceJournal {
    private struct Envelope: Codable { var version = 1; var checkpoints: [DeviceSMSIssuanceCheckpoint] = [] }
    let directory: URL
    private var unhealthy = false
    private let localLock = NSLock()
    #if SWITCH_DEVICE_SMS_TESTS
    var fault: ((String) throws -> Void)?
    #endif
    init(directory: URL) { self.directory = directory }
    private func checked(_ result: Int32) throws {
        guard result == 0 else { throw DeviceSMSJournalError.unavailable }
    }
    private func validateFD(_ fd: Int32, directory: Bool = false) throws {
        var info = stat()
        try checked(fstat(fd, &info))
        guard info.st_uid == getuid(), info.st_mode & 0o077 == 0,
              directory ? (info.st_mode & S_IFMT == S_IFDIR) : (info.st_mode & S_IFMT == S_IFREG && info.st_nlink == 1)
        else { throw DeviceSMSJournalError.unavailable }
    }
    private func readSnapshot(_ dir: Int32) throws -> Data {
        let fd = openat(dir, "issuance.json", O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw DeviceSMSJournalError.unavailable }
        defer { close(fd) }
        try validateFD(fd)
        var info = stat()
        try checked(fstat(fd, &info))
        guard info.st_size >= 0 && info.st_size <= 8 * 1024 * 1024 else { throw DeviceSMSJournalError.capacity }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 16_384)
        while true {
            let count = Darwin.read(fd, &buffer, buffer.count)
            if count < 0 && errno == EINTR { continue }
            guard count >= 0 else { throw DeviceSMSJournalError.unavailable }
            if count == 0 { break }
            data.append(contentsOf: buffer.prefix(count))
            guard data.count <= 8 * 1024 * 1024 else { throw DeviceSMSJournalError.capacity }
        }
        return data
    }
    private func writeSnapshot(_ data: Data, directory dir: Int32) throws {
        guard data.count <= 8 * 1024 * 1024 else { throw DeviceSMSJournalError.capacity }
        let name = ".issuance-" + UUID().uuidString
        let fd = openat(dir, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw DeviceSMSJournalError.unavailable }
        defer { close(fd); unlinkat(dir, name, 0) }
        try validateFD(fd)
        try data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let count = Darwin.write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { throw DeviceSMSJournalError.unavailable }
                offset += count
            }
        }
        try checked(fsync(fd))
        #if SWITCH_DEVICE_SMS_TESTS
        try fault?("before-rename")
        #endif
        try checked(renameat(dir, name, dir, "issuance.json"))
        #if SWITCH_DEVICE_SMS_TESTS
        try fault?("after-rename")
        #endif
        try checked(fsync(dir))
        guard try readSnapshot(dir) == data else { throw DeviceSMSJournalError.unavailable }
    }
    private func transaction<T>(_ body: (inout [DeviceSMSIssuanceCheckpoint]) throws -> T) throws -> T {
        localLock.lock()
        defer { localLock.unlock() }
        guard !unhealthy else { throw DeviceSMSJournalError.unavailable }
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700])
            let dir = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard dir >= 0 else { throw DeviceSMSJournalError.unavailable }
            defer { close(dir) }
            try validateFD(dir, directory: true)
            let parent = open(directory.deletingLastPathComponent().path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard parent >= 0 else { throw DeviceSMSJournalError.unavailable }
            defer { close(parent) }
            try checked(fsync(parent)) // Persist the newly created journal directory itself.
            var newLock = false
            var lock = openat(dir, "issuance.lock", O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
            if lock >= 0 { newLock = true }
            else if errno == EEXIST { lock = openat(dir, "issuance.lock", O_RDWR | O_NOFOLLOW | O_CLOEXEC) }
            guard lock >= 0 else { throw DeviceSMSJournalError.unavailable }
            defer { close(lock) }
            try validateFD(lock)
            while flock(lock, LOCK_EX) != 0 {
                guard errno == EINTR else { throw DeviceSMSJournalError.unavailable }
            }
            defer { flock(lock, LOCK_UN) }
            if newLock {
                var info = stat()
                guard fstatat(dir, "issuance.json", &info, AT_SYMLINK_NOFOLLOW) != 0 && errno == ENOENT
                else { throw DeviceSMSJournalError.unavailable }
                try checked(fsync(lock))
                try writeSnapshot(JSONEncoder().encode(Envelope()), directory: dir)
            }
            let old = try readSnapshot(dir)
            var envelope = try JSONDecoder().decode(Envelope.self, from: old)
            guard envelope.version == 1, envelope.checkpoints.count <= 64 else { throw DeviceSMSJournalError.invalid }
            var ids = Set<String>()
            for checkpoint in envelope.checkpoints {
                try checkpoint.validate()
                guard ids.insert(try checkpoint.request.clientRequestID).inserted else { throw DeviceSMSJournalError.invalid }
            }
            let previous = envelope.checkpoints
            let result = try body(&envelope.checkpoints)
            guard envelope.checkpoints.count <= 64 else { throw DeviceSMSJournalError.capacity }
            if previous != envelope.checkpoints {
                let encoder = JSONEncoder()
                encoder.outputFormatting = [.sortedKeys]
                try writeSnapshot(encoder.encode(envelope), directory: dir)
            }
            return result
        } catch DeviceSMSJournalError.conflict { throw DeviceSMSJournalError.conflict }
        catch DeviceSMSJournalError.capacity { throw DeviceSMSJournalError.capacity }
        catch { unhealthy = true; throw error }
    }
    func entries() throws -> [DeviceSMSIssuanceCheckpoint] { try transaction { $0 } }
    func checkpoint(id: String) throws -> DeviceSMSIssuanceCheckpoint? {
        try transaction { rows in try rows.first { try $0.request.clientRequestID == id } }
    }
    func insert(_ candidate: DeviceSMSIssuanceCheckpoint, explicitNew: Bool) throws -> DeviceSMSIssuanceCheckpoint {
        try candidate.validate()
        return try transaction { rows in
            let request = try candidate.request
            if !explicitNew, let existing = try rows.first(where: { try $0.samePayload(phone: request.phone, message: request.message, device: request.device, subscription: request.expectedSubscriptionID) }) { return existing }
            if let existing = try rows.first(where: { try $0.request.clientRequestID == request.clientRequestID }) {
                guard existing.sameBytes(candidate) else { throw DeviceSMSJournalError.conflict }
                return existing
            }
            rows.append(candidate)
            return candidate
        }
    }
    func advance(_ checkpoint: DeviceSMSIssuanceCheckpoint, to phase: DeviceSMSIssuanceCheckpoint.Phase) throws -> DeviceSMSIssuanceCheckpoint {
        try transaction { rows in
            let id = try checkpoint.request.clientRequestID
            guard let index = try rows.firstIndex(where: { try $0.request.clientRequestID == id }), rows[index].sameBytes(checkpoint) else { throw DeviceSMSJournalError.invalid }
            let old = rows[index].phase
            let allowed: Bool
            switch phase {
            case .candidate: allowed = false
            case .reserved: allowed = old == .candidate || old == .reserved
            case .prepared: allowed = old == .candidate || old == .reserved || old == .prepared
            case .dispatched: allowed = old == .prepared
            case .unresolved: allowed = true
            }
            guard allowed else { throw DeviceSMSJournalError.conflict }
            rows[index].phase = phase
            return rows[index]
        }
    }
    func confirmTerminal(_ record: DeviceSMSIntent) throws {
        guard (record.state == .accepted && record.archiveConfirmed) || record.state == .cancelled else { return }
        try transaction { rows in
            guard let index = try rows.firstIndex(where: { try $0.request.clientRequestID == record.id }) else { return }
            let checkpoint = rows[index]
            // A same-sequence CAS winner is not proof about the loser's nonce.
            guard try record.matches(checkpoint.request),
                  try record.intentNonce == checkpoint.reserve.intentNonce else { return }
            rows.remove(at: index)
        }
    }
}
