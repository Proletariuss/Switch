import Foundation
import CryptoKit
import Darwin
import Darwin.crt_externs

// Identity compares decoded Unicode scalars exactly. Swift String == instead
// treats canonically equivalent scalar sequences as equal. UTF-8 is a unique
// encoding for a valid scalar sequence; JSON spelling/escapes are already gone.
private func acceptanceExactString(_ lhs: String, _ rhs: String) -> Bool {
    lhs.utf8.elementsEqual(rhs.utf8)
}

// Phase A is deliberately unreachable from SwitchEntry/AcceptanceBootstrap.
// All operations below are pure except the explicitly called argv capture adapter.
// These values do not confer capsule, filesystem, connected-peer or cleanup authority.
enum AppRawEntry: Equatable {
    case production
    case candidate(CanonicalAppCapsulePath)
    case reject(StableEntryCode)

    static func classify(_ argv: [[UInt8]]) -> AppRawEntry {
        let prefixes = [Array("--switch-acceptance".utf8), Array("-switch-acceptance".utf8)]
        guard argv.dropFirst().contains(where: { arg in prefixes.contains { arg.starts(with: $0) } }) else {
            return .production
        }
        guard argv.count == 2, argv.allSatisfy({ !$0.contains(0) && String(bytes: $0, encoding: .utf8) != nil }) else {
            return .reject(.invalidArguments)
        }
        let option = Array("--switch-acceptance-capsule=".utf8)
        guard argv[1].starts(with: option),
              let path = String(bytes: argv[1].dropFirst(option.count), encoding: .utf8),
              let canonical = CanonicalAppCapsulePath(path) else { return .reject(.invalidArguments) }
        return .candidate(canonical)
    }

    // No call site in shipping entry. C argv is copied without lossy UTF-8 conversion.
    static func captureDarwinArguments() throws -> [[UInt8]] {
        let count = Int(_NSGetArgc().pointee)
        guard count > 0, let values = _NSGetArgv().pointee else { throw StableEntryCode.invalidArguments }
        return try (0..<count).map { index in
            guard let value = values[index] else { throw StableEntryCode.invalidArguments }
            let size = strlen(value)
            return Array(UnsafeRawBufferPointer(start: value, count: size))
        }
    }
}

enum StableEntryCode: Int, Error { case invalidArguments = 78 }
struct CanonicalAppCapsulePath: Equatable {
    let value: String
    static func == (lhs: Self, rhs: Self) -> Bool { acceptanceExactString(lhs.value, rhs.value) }
    fileprivate init?(_ path: String) {
        guard path.hasPrefix("/"), !path.utf8.contains(0), !path.hasSuffix("/"),
              path.split(separator: "/", omittingEmptySubsequences: false).dropFirst().allSatisfy({
                  !$0.isEmpty && $0 != "." && $0 != ".."
              }), path.split(separator: "/").last == "app.json" else { return nil }
        value = path
    }
}

enum AcceptanceParseFault: String, Error {
    case size, malformedJSON, duplicateKey, exactFields, wrongType, wrongValue
    case missingInventoryAnchor, invalidInventoryAnchor, inventoryDigestMismatch
    case invalidExpectation, httpStatus
}

// Exact scalar object grammar, not Foundation's last-key-wins object decoding.
// Numeric tokens preserve JSON integer syntax: 1.0/1e0/true never become integer 1.
private enum AcceptanceScalar: Equatable {
    case string(String), integer(Int64), bool(Bool)
    static func == (lhs: Self, rhs: Self) -> Bool {
        switch (lhs, rhs) {
        case (.string(let a), .string(let b)): return acceptanceExactString(a, b)
        case (.integer(let a), .integer(let b)): return a == b
        case (.bool(let a), .bool(let b)): return a == b
        default: return false
        }
    }
}
private struct AcceptanceStrictObject {
    private let bytes: [UInt8]
    private var index = 0
    private init(_ data: Data) throws {
        guard !data.isEmpty, data.count <= 16384 else { throw AcceptanceParseFault.size }
        bytes = Array(data)
    }
    static func decode(_ data: Data, fields: Set<String>) throws -> [String: AcceptanceScalar] {
        var parser = try Self(data)
        let object = try parser.object()
        // All schema keys are fixed lowercase ASCII: none has a distinct
        // canonically equivalent scalar sequence. Dictionary/Set key equality
        // cannot admit a Unicode alias of an allowed key; decoded duplicates
        // (including JSON escape aliases) remain rejected before insertion.
        guard Set(object.keys) == fields else { throw AcceptanceParseFault.exactFields }
        return object
    }
    private mutating func whitespace() {
        while index < bytes.count && [UInt8(32), 9, 10, 13].contains(bytes[index]) { index += 1 }
    }
    private mutating func consume(_ byte: UInt8) throws {
        whitespace()
        guard index < bytes.count, bytes[index] == byte else { throw AcceptanceParseFault.malformedJSON }
        index += 1
    }
    private mutating func string() throws -> String {
        whitespace()
        let start = index
        try consume(34)
        var escaped = false
        while index < bytes.count {
            let byte = bytes[index]
            index += 1
            if !escaped && byte == 34 {
                do { return try JSONDecoder().decode(String.self, from: Data(bytes[start..<index])) }
                catch { throw AcceptanceParseFault.malformedJSON }
            }
            if !escaped && byte == 92 { escaped = true } else { escaped = false }
        }
        throw AcceptanceParseFault.malformedJSON
    }
    private mutating func literal(_ text: String) throws {
        let token = Array(text.utf8)
        guard bytes[index...].starts(with: token) else { throw AcceptanceParseFault.malformedJSON }
        index += token.count
    }
    private mutating func scalar() throws -> AcceptanceScalar {
        whitespace()
        guard index < bytes.count else { throw AcceptanceParseFault.malformedJSON }
        if bytes[index] == 34 { return .string(try string()) }
        if bytes[index] == 116 { try literal("true"); return .bool(true) }
        if bytes[index] == 102 { try literal("false"); return .bool(false) }
        let start = index
        if bytes[index] == 45 { index += 1 }
        let digits = index
        while index < bytes.count, bytes[index] >= 48, bytes[index] <= 57 { index += 1 }
        guard index > digits, !(index - digits > 1 && bytes[digits] == 48),
              let value = Int64(String(decoding: bytes[start..<index], as: UTF8.self)) else {
            throw AcceptanceParseFault.malformedJSON
        }
        return .integer(value)
    }
    private mutating func object() throws -> [String: AcceptanceScalar] {
        try consume(123)
        var values: [String: AcceptanceScalar] = [:]
        whitespace()
        if index < bytes.count && bytes[index] == 125 { index += 1 }
        else {
            while true {
                let key = try string()
                guard values[key] == nil else { throw AcceptanceParseFault.duplicateKey }
                try consume(58)
                values[key] = try scalar()
                whitespace()
                guard index < bytes.count else { throw AcceptanceParseFault.malformedJSON }
                if bytes[index] == 125 { index += 1; break }
                try consume(44)
            }
        }
        whitespace()
        guard index == bytes.count else { throw AcceptanceParseFault.malformedJSON }
        return values
    }
}
private func acceptanceHex(_ value: String, count: Int) -> Bool {
    value.utf8.count == count && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
}
private func acceptanceString(_ object: [String: AcceptanceScalar], _ key: String) throws -> String {
    guard case .string(let value) = object[key] else { throw AcceptanceParseFault.wrongType }
    guard !value.isEmpty else { throw AcceptanceParseFault.wrongValue }
    return value
}
private func acceptanceRequire(_ object: [String: AcceptanceScalar], _ key: String, _ value: AcceptanceScalar) throws {
    guard object[key] == value else { throw AcceptanceParseFault.wrongValue }
}

enum AcceptanceInventoryAnchor {
    // No generator/anchor exists in Phase A. There is no env, preference or response fallback.
    static var sha256: String? { nil }
}
struct AcceptanceBuildInventory: Equatable {
    let sourceDigest: String
    let version: String
    let appReleaseVersion: String
    let buildNumber: String
    let inventorySHA256: String
    static func == (lhs: Self, rhs: Self) -> Bool {
        acceptanceExactString(lhs.sourceDigest, rhs.sourceDigest) &&
        acceptanceExactString(lhs.version, rhs.version) &&
        acceptanceExactString(lhs.appReleaseVersion, rhs.appReleaseVersion) &&
        acceptanceExactString(lhs.buildNumber, rhs.buildNumber) &&
        acceptanceExactString(lhs.inventorySHA256, rhs.inventorySHA256)
    }
    private init(sourceDigest: String, version: String, appReleaseVersion: String, buildNumber: String, digest: String) {
        self.sourceDigest = sourceDigest; self.version = version
        self.appReleaseVersion = appReleaseVersion; self.buildNumber = buildNumber; inventorySHA256 = digest
    }
    static func decode(_ bytes: Data) throws -> Self { try decode(bytes, anchor: AcceptanceInventoryAnchor.sha256) }
    #if SWITCH_ACCEPTANCE_PHASE_A_TESTS
    static func decodeFixture(_ bytes: Data, anchor: String?) throws -> Self { try decode(bytes, anchor: anchor) }
    #endif
    private static func decode(_ bytes: Data, anchor: String?) throws -> Self {
        guard let anchor else { throw AcceptanceParseFault.missingInventoryAnchor }
        guard acceptanceHex(anchor, count: 64) else { throw AcceptanceParseFault.invalidInventoryAnchor }
        guard !bytes.isEmpty, bytes.count <= 16384 else { throw AcceptanceParseFault.size }
        let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        guard digest == anchor else { throw AcceptanceParseFault.inventoryDigestMismatch }
        let object = try AcceptanceStrictObject.decode(bytes, fields: ["schema_version", "source_digest", "version", "app_release_version", "build_number"])
        try acceptanceRequire(object, "schema_version", .integer(1))
        let source = try acceptanceString(object, "source_digest")
        guard acceptanceHex(source, count: 64) else { throw AcceptanceParseFault.wrongValue }
        return try Self(sourceDigest: source, version: acceptanceString(object, "version"),
                        appReleaseVersion: acceptanceString(object, "app_release_version"),
                        buildNumber: acceptanceString(object, "build_number"), digest: digest)
    }
}

// Expectations are explicit data, never derived from the response being checked.
// Their PIDs are scalar assertions only: no actual-FD peer proof is implemented here.
struct AcceptanceDTOExpectation {
    let runID: String
    let appPID: Int32
    let daemonPID: Int32
    let observedPeerPID: Int32
    let inventory: AcceptanceBuildInventory
    init(runID: String, appPID: Int32, daemonPID: Int32, observedPeerPID: Int32, inventory: AcceptanceBuildInventory) throws {
        guard acceptanceHex(runID, count: 32), appPID > 0, daemonPID > 0, observedPeerPID == daemonPID else {
            throw AcceptanceParseFault.invalidExpectation
        }
        self.runID = runID; self.appPID = appPID; self.daemonPID = daemonPID
        self.observedPeerPID = observedPeerPID; self.inventory = inventory
    }
}
struct AcceptanceRuntimeDTO: Equatable {
    let runID: String; let appPID: Int32; let daemonPID: Int32
    let sourceDigest: String; let version: String; let appReleaseVersion: String; let buildNumber: String
    static func == (lhs: Self, rhs: Self) -> Bool {
        acceptanceExactString(lhs.runID, rhs.runID) && lhs.appPID == rhs.appPID && lhs.daemonPID == rhs.daemonPID &&
        acceptanceExactString(lhs.sourceDigest, rhs.sourceDigest) && acceptanceExactString(lhs.version, rhs.version) &&
        acceptanceExactString(lhs.appReleaseVersion, rhs.appReleaseVersion) && acceptanceExactString(lhs.buildNumber, rhs.buildNumber)
    }
}
struct AcceptanceHealthDTO: Equatable {
    let runID: String; let appPID: Int32; let daemonPID: Int32
    static func == (lhs: Self, rhs: Self) -> Bool {
        acceptanceExactString(lhs.runID, rhs.runID) && lhs.appPID == rhs.appPID && lhs.daemonPID == rhs.daemonPID
    }
}
enum AcceptanceDTOParser {
    // Computed values avoid global/static initialization and observable lazy state.
    private static var commonKeys: Set<String> {
        ["schema_version", "ok", "mode", "run_id", "daemon_pid", "app_pid", "hardware_owner",
         "hardware_ownership", "hardware_ready", "stores_opened", "providers_started", "external_network_enabled"]
    }
    private static func common(_ data: Data, status: Int, extra: Set<String>, expected: AcceptanceDTOExpectation) throws -> [String: AcceptanceScalar] {
        guard status == 200 else { throw AcceptanceParseFault.httpStatus }
        let object = try AcceptanceStrictObject.decode(data, fields: commonKeys.union(extra))
        try acceptanceRequire(object, "schema_version", .integer(1))
        try acceptanceRequire(object, "ok", .bool(true))
        try acceptanceRequire(object, "mode", .string("no-device-acceptance"))
        try acceptanceRequire(object, "hardware_owner", .string("disabled_no_device_acceptance"))
        try acceptanceRequire(object, "run_id", .string(expected.runID))
        try acceptanceRequire(object, "app_pid", .integer(Int64(expected.appPID)))
        try acceptanceRequire(object, "daemon_pid", .integer(Int64(expected.daemonPID)))
        for key in ["hardware_ownership", "hardware_ready", "stores_opened", "providers_started", "external_network_enabled"] {
            try acceptanceRequire(object, key, .bool(false))
        }
        return object
    }
    static func runtime(_ data: Data, status: Int, expected: AcceptanceDTOExpectation) throws -> AcceptanceRuntimeDTO {
        let object = try common(data, status: status, extra: ["backend", "source_digest", "version", "app_release_version", "build_number"], expected: expected)
        try acceptanceRequire(object, "backend", .string("rust"))
        let inventory = expected.inventory
        for (key, value) in [("source_digest", inventory.sourceDigest), ("version", inventory.version),
                             ("app_release_version", inventory.appReleaseVersion), ("build_number", inventory.buildNumber)] {
            try acceptanceRequire(object, key, .string(value))
        }
        return AcceptanceRuntimeDTO(runID: expected.runID, appPID: expected.appPID, daemonPID: expected.daemonPID,
                                    sourceDigest: inventory.sourceDigest, version: inventory.version,
                                    appReleaseVersion: inventory.appReleaseVersion, buildNumber: inventory.buildNumber)
    }
    static func health(_ data: Data, status: Int, expected: AcceptanceDTOExpectation) throws -> AcceptanceHealthDTO {
        let object = try common(data, status: status, extra: ["service_ready"], expected: expected)
        try acceptanceRequire(object, "service_ready", .bool(true))
        return AcceptanceHealthDTO(runID: expected.runID, appPID: expected.appPID, daemonPID: expected.daemonPID)
    }
}

// Pure evidence summaries. No type below writes files, sends signals, clears a
// fence, launches a process or issues a receipt. Runtime workers remain future work.
struct AcceptanceEvidenceIdentity: Equatable {
    let runID: String; let pid: Int32; let generation: UInt64
    static func == (lhs: Self, rhs: Self) -> Bool {
        acceptanceExactString(lhs.runID, rhs.runID) && lhs.pid == rhs.pid && lhs.generation == rhs.generation
    }
    var valid: Bool { acceptanceHex(runID, count: 32) && pid > 0 && generation > 0 }
}
enum AcceptanceIOPhase: String {
    case read, destinationWrite, destinationFlush, destinationClose, readerClose
    case append, rotation, finalFlush, fileSync, logClose, directorySync
    case fenceWrite, fenceReadback, fenceMode, fenceSync, fenceClose, fenceClear, fenceAbsent
    case daemonRetirement, contextProof, inventoryProof, guardianCustody, workerJoin, faultOverflow
}
struct StableIOFault: Equatable { let phase: AcceptanceIOPhase; let errno: Int32? }
struct AcceptanceFaults: Equatable {
    let entries: [StableIOFault]
    let overflowCount: UInt64
    init(_ faults: [StableIOFault], alreadyOverflowed: UInt64 = 0) {
        entries = Array(faults.prefix(64))
        let extra = UInt64(max(0, faults.count - 64))
        let sum = alreadyOverflowed.addingReportingOverflow(extra)
        overflowCount = sum.overflow ? UInt64.max : sum.partialValue
    }
    var clean: Bool { entries.isEmpty && overflowCount == 0 }
}
enum AcceptanceCheckedStep: Equatable {
    case succeeded, notPerformed, failed(StableIOFault), uncertain(StableIOFault)
    var clean: Bool { self == .succeeded }
}
enum AcceptanceStreamKind { case stdout, stderr }
enum AcceptanceStreamEnd: Equatable { case eof, readFailure(StableIOFault), cancelled, deadline }
enum AcceptanceWorkerJoin { case joined, stillOwned }
enum AcceptanceDestinationFlush: Equatable {
    case pipeDeliveryAcknowledged, fileFlush(AcceptanceCheckedStep), notPerformed
    var clean: Bool {
        switch self { case .pipeDeliveryAcknowledged: return true
        case .fileFlush(let result): return result.clean
        case .notPerformed: return false }
    }
}
struct AcceptanceDestinationReport {
    let writes: AcceptanceCheckedStep
    let flush: AcceptanceDestinationFlush
    let ownedClose: AcceptanceCheckedStep
    var clean: Bool { writes.clean && flush.clean && ownedClose.clean }
}
struct AcceptancePipeOutcome {
    let identity: AcceptanceEvidenceIdentity; let stream: AcceptanceStreamKind
    let end: AcceptanceStreamEnd; let worker: AcceptanceWorkerJoin
    let bytesRead: UInt64; let bytesDelivered: UInt64; let logBytesAcknowledged: UInt64
    let destination: AcceptanceDestinationReport; let readerClose: AcceptanceCheckedStep
    let faults: AcceptanceFaults
    var clean: Bool {
        identity.valid && end == .eof && worker == .joined && bytesRead == bytesDelivered &&
        bytesRead == logBytesAcknowledged && destination.clean && readerClose.clean && faults.clean
    }
}
struct AcceptanceLogOutcome {
    let identity: AcceptanceEvidenceIdentity; let worker: AcceptanceWorkerJoin
    let append: AcceptanceCheckedStep; let rotation: AcceptanceCheckedStep
    let finalFlush: AcceptanceCheckedStep; let fileSync: AcceptanceCheckedStep
    let ownedClose: AcceptanceCheckedStep; let directorySync: AcceptanceCheckedStep
    let bytesAcknowledged: UInt64; let finalSequence: UInt64; let lastAcknowledgedSequence: UInt64
    let allSequencesAcknowledged: Bool; let faults: AcceptanceFaults
    var clean: Bool {
        identity.valid && worker == .joined && append.clean && rotation.clean && finalFlush.clean && fileSync.clean &&
        ownedClose.clean && directorySync.clean && finalSequence == lastAcknowledgedSequence &&
        allSequencesAcknowledged && faults.clean
    }
}
struct AcceptanceFenceBinding: Equatable {
    let capsuleSHA256: String
    let session: String
    let reason: String
    let timestampUnixMS: Int64
    static func == (lhs: Self, rhs: Self) -> Bool {
        acceptanceExactString(lhs.capsuleSHA256, rhs.capsuleSHA256) &&
        acceptanceExactString(lhs.session, rhs.session) && acceptanceExactString(lhs.reason, rhs.reason) &&
        lhs.timestampUnixMS == rhs.timestampUnixMS
    }
    var valid: Bool {
        acceptanceHex(capsuleSHA256, count: 64) && !session.isEmpty && !reason.isEmpty && timestampUnixMS > 0
    }
}
struct AcceptanceFenceReport {
    let identity: AcceptanceEvidenceIdentity
    let binding: AcceptanceFenceBinding
    let write: AcceptanceCheckedStep; let readback: AcceptanceCheckedStep; let mode: AcceptanceCheckedStep
    let fileSync: AcceptanceCheckedStep; let ownedClose: AcceptanceCheckedStep; let persistDirectorySync: AcceptanceCheckedStep
    let clear: AcceptanceCheckedStep; let absence: AcceptanceCheckedStep; let clearDirectorySync: AcceptanceCheckedStep
    var clean: Bool {
        identity.valid && binding.valid && [write, readback, mode, fileSync, ownedClose, persistDirectorySync, clear, absence, clearDirectorySync].allSatisfy(\.clean)
    }
}
struct AcceptanceFinalProof {
    let identity: AcceptanceEvidenceIdentity
    let result: AcceptanceCheckedStep
}
enum AcceptanceChildExit: Equatable { case requestedNormal(Int32), unexpectedNormal(Int32), signalled(Int32), forced, notObserved }
enum AcceptanceCleanupAssessment: Equatable { case evidenceComplete, failed }
struct AcceptanceChildCleanupReport {
    let identity: AcceptanceEvidenceIdentity; let exit: AcceptanceChildExit
    let expectedFenceBinding: AcceptanceFenceBinding
    let stdout: AcceptancePipeOutcome; let stderr: AcceptancePipeOutcome; let writer: AcceptanceLogOutcome
    let fence: AcceptanceFenceReport
    let daemonRetirement: AcceptanceFinalProof; let context: AcceptanceFinalProof
    let inventory: AcceptanceFinalProof; let custody: AcceptanceFinalProof
    let earlierFaults: AcceptanceFaults
    var assessment: AcceptanceCleanupAssessment {
        let proofs = [daemonRetirement, context, inventory, custody]
        let total = stdout.logBytesAcknowledged.addingReportingOverflow(stderr.logBytesAcknowledged)
        guard identity.valid, exit == .requestedNormal(0), earlierFaults.clean,
              stdout.identity == identity, stderr.identity == identity, writer.identity == identity, fence.identity == identity,
              stdout.stream == .stdout, stderr.stream == .stderr,
              stdout.clean, stderr.clean, writer.clean, fence.clean, fence.binding == expectedFenceBinding,
              !total.overflow, total.partialValue == writer.bytesAcknowledged,
              proofs.allSatisfy({ $0.identity == identity && $0.result.clean }) else { return .failed }
        return .evidenceComplete
    }
}

// Compile-time signatures only. Phase A never creates or awaits any worker.
typealias AcceptancePipeWorker = Task<AcceptancePipeOutcome, Error>
typealias AcceptanceLogWorker = Task<AcceptanceLogOutcome, Error>
