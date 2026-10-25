import Foundation
import Darwin

enum LocalBackendAuthentication { static let appToken = "isolated"; static let headerName = "X-Test-Token" }
final class UnixSocketURLProtocol: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() { fatalError("fake export API must never invoke a real transport") }
    override func stopLoading() {}
}
struct Failure: Error, CustomStringConvertible { let description: String }
func expect(_ condition: Bool, _ message: String) throws {
    if !condition { throw Failure(description: message) }
}
actor FakeExportAPI: SMSArchiveExportAPI {
    enum Mode { case normal, short, oversized, extraTail, secondFailure, secondWait, invalidTotal, invalidToken, cleanupFailure }
    let bytes: Data
    let mode: Mode
    private(set) var offsets: [UInt64] = []
    private(set) var removals: [String] = []
    private(set) var removeWasCancelled = false
    private(set) var prepared = 0
    init(bytes: Data, mode: Mode = .normal) { self.bytes = bytes; self.mode = mode }
    func prepare(_ request: SMSArchiveExportRequest) async throws -> SMSArchiveExportMetadata {
        prepared += 1
        return SMSArchiveExportMetadata(schemaVersion: 1, token: mode == .invalidToken ? "../private" : String(repeating: "a", count: 43),
                                        totalBytes: mode == .invalidTotal ? SMSArchiveExporter.outputLimit + 1 : UInt64(bytes.count),
                                        messageCount: 42, chunkBytes: SMSArchiveExporter.chunkLimit, expiresAtMS: 1_791_000_600_000)
    }
    func chunk(token: String, offset: UInt64) async throws -> Data {
        offsets.append(offset)
        switch mode {
        case .short where offset == 0: return Data([1])
        case .oversized where offset == 0: return Data(repeating: 0, count: SMSArchiveExporter.chunkLimit + 1)
        case .extraTail where offset == bytes.count: return Data([1])
        case .secondFailure where offset > 0: throw Failure(description: "connection lost")
        case .secondWait where offset > 0: try await Task.sleep(nanoseconds: 30_000_000_000)
        default: break
        }
        let start = Int(offset), end = min(bytes.count, start + SMSArchiveExporter.chunkLimit)
        return bytes.subdata(in: start..<end)
    }
    func remove(token: String) async throws {
        removeWasCancelled = Task.isCancelled
        removals.append(token)
        if mode == .cleanupFailure { throw Failure(description: "cleanup unavailable") }
    }
}
struct Fixture {
    let root: URL
    init() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("switch-export-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
    }
    var target: URL { root.appendingPathComponent("archive.json") }
    func clean() { try? FileManager.default.removeItem(at: root) }
    func assertNoTemporary() throws {
        try expect(try FileManager.default.contentsOfDirectory(atPath: root.path).allSatisfy { !$0.hasPrefix(".switch-sms-export-") }, "temporary export survived")
    }
}
@main enum ExportTests {
    static func main() async throws {
        let bytes = Data(("{\"body\":\"" + String(repeating: "测试😀", count: 90_000) + "\"}").utf8)
        do {
            let f = try Fixture(); defer { f.clean() }
            try Data("existing file".utf8).write(to: f.target)
            let api = FakeExportAPI(bytes: bytes)
            let result = try await SMSArchiveExporter(api: api).save(.init(), to: f.target)
            try expect(try Data(contentsOf: f.target) == bytes, "chunk reassembly changed data")
            try expect(result.totalBytes == bytes.count && result.messageCount == 42, "summary changed")
            let offsets = await api.offsets
            try expect(offsets == [0, 262_144, 524_288, 786_432, UInt64(bytes.count)], "offsets or EOF contract wrong")
            let mode = try FileManager.default.attributesOfItem(atPath: f.target.path)[.posixPermissions] as? Int
            try expect(mode == 0o600, "final content was not private")
            try expect(await api.removals.count == 1, "successful export did not clean server token")
            try f.assertNoTemporary()
        }
        print("PASS: bounded exact chunks + EOF, private atomic replacement, server cleanup")
        for mode in [FakeExportAPI.Mode.short, .oversized, .extraTail, .secondFailure, .invalidTotal, .invalidToken] {
            let f = try Fixture(); defer { f.clean() }
            let original = Data("keep old file".utf8); try original.write(to: f.target)
            let api = FakeExportAPI(bytes: bytes, mode: mode)
            do { _ = try await SMSArchiveExporter(api: api).save(.init(), to: f.target); throw Failure(description: "invalid transfer succeeded") }
            catch is SMSArchiveExportError {} catch let e as Failure { if e.description != "connection lost" { throw e } }
            try expect(try Data(contentsOf: f.target) == original, "failed transfer replaced original")
            try expect(await api.removals.count == (mode == .invalidToken ? 0 : 1), "failed transfer token cleanup wrong")
            try f.assertNoTemporary()
        }
        print("PASS: truncation, extra bytes, malformed metadata and interrupted transfers preserve destination")
        do {
            let f = try Fixture(); defer { f.clean() }; let original = Data("keep on cancel".utf8); try original.write(to: f.target)
            let api = FakeExportAPI(bytes: bytes, mode: .secondWait)
            let exporter = SMSArchiveExporter(api: api)
            let task = Task { try await exporter.save(.init(), to: f.target) }
            while await api.offsets.count < 2 { await Task.yield() }
            do {
                _ = try await exporter.save(.init(), to: f.root.appendingPathComponent("second.json"))
                throw Failure(description: "overlapping export accepted")
            } catch SMSArchiveExportError.busy {}
            try expect(await api.prepared == 1, "overlapping export prepared another snapshot")
            task.cancel()
            do { _ = try await task.value; throw Failure(description: "cancelled export succeeded") } catch is CancellationError {}
            try expect(try Data(contentsOf: f.target) == original, "cancel replaced target")
            try expect(await api.removals.count == 1, "cancel did not clean token")
            try expect(await !api.removeWasCancelled, "DELETE inherited cancellation")
            try f.assertNoTemporary()
        }
        print("PASS: single active export, cancellation removes partial file and uses uncancelled DELETE")
        do {
            let f = try Fixture(); defer { f.clean() }; let api = FakeExportAPI(bytes: bytes)
            let real = f.root.appendingPathComponent("real.json"); try Data("keep real".utf8).write(to: real)
            try FileManager.default.createSymbolicLink(at: f.target, withDestinationURL: real)
            do { _ = try await SMSArchiveExporter(api: api).save(.init(), to: f.target); throw Failure(description: "symlink accepted") } catch is SMSArchiveExportError {}
            try expect(await api.prepared == 0, "bad destination triggered preparation")
            try expect(try String(contentsOf: real, encoding: .utf8) == "keep real", "symlink target changed")
            let missing = f.root.appendingPathComponent("missing/out.json")
            do { _ = try await SMSArchiveExporter(api: api).save(.init(), to: missing); throw Failure(description: "missing directory accepted") } catch is SMSArchiveExportError {}
            try expect(await api.prepared == 0, "unwritable destination triggered preparation")
            try f.assertNoTemporary()
        }
        print("PASS: invalid filesystem targets fail before any archive request")
        do {
            let f = try Fixture(); defer { f.clean() }; let api = FakeExportAPI(bytes: bytes, mode: .cleanupFailure)
            _ = try await SMSArchiveExporter(api: api).save(.init(), to: f.target)
            try expect(try Data(contentsOf: f.target) == bytes, "best-effort cleanup failure lost completed file")
            try f.assertNoTemporary()
        }
        print("PASS: completed export survives best-effort server cleanup failure; TTL owns fallback")
    }
}
