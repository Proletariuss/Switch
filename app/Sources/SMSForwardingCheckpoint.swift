import Foundation

@_silgen_name("switch_handoff_checkpoint_create")
private func checkpointCreate(_ path: UnsafePointer<UInt8>?, _ count: Int) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_checkpoint_destroy")
private func checkpointDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_handoff_checkpoint_io")
private func checkpointIO(_ handle: UnsafeMutableRawPointer, _ bytes: UnsafePointer<UInt8>?, _ count: Int, _ confirm: UInt8) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_checkpoint_fault_clone")
private func checkpointFaultClone(_ handle: UnsafeMutableRawPointer, _ fault: UInt8) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_authority_create")
private func authorityCreate(_ checkpoint: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_authority_destroy")
private func authorityDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_handoff_authority_local")
private func authorityLocal(_ handle: UnsafeMutableRawPointer?, _ bytes: UnsafePointer<UInt8>?, _ count: Int, _ present: UInt8) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_authority_remote")
private func authorityRemote(_ handle: UnsafeMutableRawPointer?, _ accepted: UInt8, _ snapshot: UnsafePointer<UInt8>?, _ count: Int, _ present: UInt8, _ source: UnsafePointer<UInt8>?, _ sourceCount: Int) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_validate")
private func handoffValidate(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> UnsafeMutableRawPointer?
@_silgen_name("switch_handoff_bytes_status")
private func handoffStatus(_ result: UnsafeMutableRawPointer) -> Int32
@_silgen_name("switch_handoff_bytes_length")
private func handoffLength(_ result: UnsafeMutableRawPointer) -> Int
@_silgen_name("switch_handoff_bytes_data")
private func handoffData(_ result: UnsafeMutableRawPointer) -> UnsafePointer<UInt8>?
@_silgen_name("switch_handoff_bytes_destroy")
private func handoffDestroy(_ result: UnsafeMutableRawPointer)
#if SWITCH_CHECKPOINT_TESTS
@_silgen_name("switch_handoff_test_lock")
private func handoffTestLock(_ descriptor: Int32, _ operation: Int32) -> Int32
#endif
#if SWITCH_FORWARDING_TESTS
@_silgen_name("switch_handoff_authority_pending_for_test")
private func authorityPendingForTest(_ handle: UnsafeMutableRawPointer?) -> UnsafeMutableRawPointer?
#endif

struct SMSForwardingRustFailure: Error { let code: Int32 }

private func returnedHandoffBytes(_ result: UnsafeMutableRawPointer?) throws -> Data? {
    guard let result else { throw SMSForwardingRustFailure(code: -1) }
    defer { handoffDestroy(result) }
    let status = handoffStatus(result)
    if status == 0 { return nil }
    guard status == 1 else { throw SMSForwardingRustFailure(code: status) }
    let count = handoffLength(result)
    guard (0...SMSForwardingCheckpoint.maximumBytes).contains(count) else {
        throw SMSForwardingRustFailure(code: -1)
    }
    if count == 0 { return Data() }
    guard let bytes = handoffData(result) else { throw SMSForwardingRustFailure(code: -1) }
    return Data(bytes: bytes, count: count)
}

/// Thin byte/lifetime bridge. Rust owns pinned directory/lock identities, file
/// mode and size limits, first commit and every durability confirmation.
struct SMSForwardingCheckpoint: Sendable {
    static let fileName = "sms-forwarding-swift-checkpoint-v1.json"
    static let lockName = ".sms-forwarding-swift-checkpoint-v1.lock"
    static let maximumBytes = 8 * 1024 * 1024
    enum Failure: Error, Equatable { case unavailable, busy, tooLarge }

    private final class Handle: @unchecked Sendable {
        let pointer: UnsafeMutableRawPointer
        let lock = NSLock()
        init(_ pointer: UnsafeMutableRawPointer) { self.pointer = pointer }
        deinit { checkpointDestroy(pointer) }
    }
    private let handle: Handle
    init(directory: URL) {
        // No path fallback or creation: an invalid/non-file URL gives Rust an
        // unusable path, retaining the original fail-closed initializer shape.
        let bytes = Array((directory.isFileURL ? directory.standardizedFileURL.path : "").utf8)
        guard let pointer = bytes.withUnsafeBufferPointer({ checkpointCreate($0.baseAddress, $0.count) }) else {
            preconditionFailure("Rust checkpoint allocation failed")
        }
        handle = Handle(pointer)
    }
    #if SWITCH_CHECKPOINT_TESTS
    enum TestFailure: Sendable, Equatable { case fileSync, directorySync }
    init(directory: URL, failing: TestFailure) {
        let ordinary = SMSForwardingCheckpoint(directory: directory)
        guard let pointer = checkpointFaultClone(ordinary.handle.pointer, failing == .fileSync ? 1 : 2) else {
            // Invalid roots still fail on IO, rather than acquiring a new path.
            handle = ordinary.handle
            return
        }
        handle = Handle(pointer)
    }
    static func lockFileForTesting(_ descriptor: Int32, _ operation: Int32) -> Int32 {
        handoffTestLock(descriptor, operation)
    }
    #endif

    func loadConfirmed() throws -> Data? { try io(nil) }
    func confirm(_ candidate: Data) throws -> Data {
        guard let result = try io(candidate) else { throw Failure.unavailable }
        return result
    }
    private func io(_ candidate: Data?) throws -> Data? {
        handle.lock.lock(); defer { handle.lock.unlock() }
        do {
            let data = candidate ?? Data()
            return try data.withUnsafeBytes {
                try returnedHandoffBytes(checkpointIO(handle.pointer, $0.bindMemory(to: UInt8.self).baseAddress,
                    $0.count, candidate == nil ? 0 : 1))
            }
        } catch let error as SMSForwardingRustFailure {
            switch error.code { case -2: throw Failure.busy; case -3: throw Failure.tooLarge; default: throw Failure.unavailable }
        }
    }
    fileprivate func makeAuthorityHandle() -> UnsafeMutableRawPointer? {
        handle.lock.lock(); defer { handle.lock.unlock() }
        return authorityCreate(handle.pointer)
    }
}

/// DTO decoding below is transport adaptation only. Rust validates the legacy
/// schema and returns canonical POST bytes solely after checkpoint fsync ACK.
final class SMSForwardingRustAuthority {
    private let handle: UnsafeMutableRawPointer?
    private let lock = NSLock()
    init(checkpoint: SMSForwardingCheckpoint) { handle = checkpoint.makeAuthorityHandle() }
    deinit { if let handle { authorityDestroy(handle) } }
    #if SWITCH_FORWARDING_TESTS
    func pendingForTesting() throws -> Data? {
        lock.lock(); defer { lock.unlock() }
        return try returnedHandoffBytes(authorityPendingForTest(handle))
    }
    #endif
    func local(legacy: Data?) throws -> Data? {
        lock.lock(); defer { lock.unlock() }
        let bytes = legacy ?? Data()
        return try bytes.withUnsafeBytes {
            try returnedHandoffBytes(authorityLocal(handle, $0.bindMemory(to: UInt8.self).baseAddress,
                $0.count, legacy == nil ? 0 : 1))
        }
    }
    func remote(accepted: Bool, snapshot: Data?, source: Data) throws -> Data {
        lock.lock(); defer { lock.unlock() }
        let snapshotBytes = snapshot ?? Data()
        return try snapshotBytes.withUnsafeBytes { s in
            try source.withUnsafeBytes { p in
                guard let result = try returnedHandoffBytes(authorityRemote(handle, accepted ? 1 : 0,
                    s.bindMemory(to: UInt8.self).baseAddress, s.count, snapshot == nil ? 0 : 1,
                    p.bindMemory(to: UInt8.self).baseAddress, p.count)) else {
                    throw SMSForwardingRustFailure(code: -1)
                }
                return result
            }
        }
    }
    static func validated(_ bytes: Data) throws -> Data {
        try bytes.withUnsafeBytes {
            guard let result = try returnedHandoffBytes(handoffValidate($0.bindMemory(to: UInt8.self).baseAddress, $0.count)) else {
                throw SMSForwardingRustFailure(code: -10)
            }
            return result
        }
    }
}
