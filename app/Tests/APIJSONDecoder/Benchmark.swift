import Foundation

// Test-only dependencies deny transport access; only the production synchronous
// APIClient.decodeResponse helper and real SMSModels are called.
final class UnixSocketURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() { fatalError("Network forbidden in this benchmark") }
    override func stopLoading() {}
}
enum LocalBackendAuthentication {
    static let appToken = "synthetic-test-only"
    static let headerName = "X-Proletarius-App-Token"
}
enum PrivacyMask {
    static func value(_ value: String, enabled: Bool) -> String { value }
}

// Same data and computation as SMSConversationViews.swift:4 and SMSView.swift:18.
struct Conversation: Identifiable {
    let phone: String
    var messages: [SMSItem]
    var id: String { phone }
    var lastMessage: SMSItem? { messages.last }
    var lastTimestamp: Date { messages.last?.timestamp ?? .distantPast }
    var senderForDeletion: String? {
        messages.compactMap(\.sender).first { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }
}

@inline(never)
func group(_ items: [SMSItem]) -> [Conversation] {
    Dictionary(grouping: items) { $0.sender ?? "未知号码" }
        .map { Conversation(phone: $0.key, messages: $0.value.sorted { $0.timestamp < $1.timestamp }) }
        .sorted { $0.lastTimestamp > $1.lastTimestamp }
}

struct FixtureItem: Encodable {
    let id: String
    let sender: String
    let content: String
    let code: String?
    let timestamp: String
    let module_storage: String?
    let module_index: Int?
    let archived: Bool
    let direction: String
    let device: String
}

let validFormats = [
    "2026-09-26T03:04:05Z",
    "2026-09-26T03:04:05.1Z",
    "2026-09-26T03:04:05.12Z",
    "2026-09-26T03:04:05.123Z",
    "2026-09-26T03:04:05.1234Z",
    "2026-09-26T03:04:05.12345Z",
    "2026-09-26T03:04:05.123456Z",
    "2026-09-26T03:04:05.1234567Z",
    "2026-09-26T03:04:05.12345678Z",
    "2026-09-26T03:04:05.123456789Z",
    "2026-09-26T11:04:05.123456789+08:00",
    "2026-09-25T22:04:05.123456789-05:00",
    "2024-02-29T23:59:59Z",
    "2026-09-26T03:04:05.000000001Z",
    "2026-09-26T03:04:05.999999999Z",
]
let invalidFormats = ["", "not-a-date", "2026-09-26", "2026-09-26T03:04:05.123", "2026-09-26T03:04:05.xZ"]

func fixture(_ count: Int, formats: [String] = validFormats) throws -> Data {
    let items = (0..<count).map { index in
        FixtureItem(
            id: "synthetic-\(index)", sender: "fixture-sender-\(index % 25)",
            content: "脱敏合成短信 \(index)，只用于性能测量，不含真实联系人。 Synthetic fixture content.",
            code: index % 4 == 0 ? "000000" : nil, timestamp: formats[index % formats.count],
            module_storage: index % 3 == 0 ? "SM" : nil,
            module_index: index % 3 == 0 ? index + 1 : nil,
            archived: index % 3 != 0, direction: index % 4 == 0 ? "out" : "in",
            device: index % 2 == 0 ? "reader" : "dji")
    }
    return try JSONEncoder().encode(items)
}

struct Stats: Codable {
    let count: Int
    let medianMS: Double
    let p95MS: Double
    let minimumMS: Double
    let maximumMS: Double
    let rawMS: [Double]
    init(_ samples: [Double]) {
        let sorted = samples.sorted()
        count = sorted.count
        medianMS = sorted.count % 2 == 0
            ? (sorted[sorted.count / 2 - 1] + sorted[sorted.count / 2]) / 2
            : sorted[sorted.count / 2]
        p95MS = sorted[Int(ceil(Double(sorted.count) * 0.95)) - 1]
        minimumMS = sorted.first!
        maximumMS = sorted.last!
        rawMS = samples
    }
}

struct CaseResult: Codable {
    let fixtureCount: Int
    let fixtureBytes: Int
    let baselineDecode: Stats
    let productionDecodeIncludingSetup: Stats
    let groupOnly: Stats
    let baselineDecodeAndGroup: Stats
    let productionDecodeAndGroupIncludingSetup: Stats
    let p95DecodeSavedMS: Double
    let p95DecodeSavedPercent: Double
    let passesGate: Bool
}
struct Report: Codable {
    let generatedAt: String
    let warmups: Int
    let samples: Int
    let ordering: String
    let scope: String
    let correctnessCases: Int
    let stressConcurrentDecodes: Int
    let cases: [CaseResult]
    let checksum: Double
}

@inline(never)
func timed(_ work: () throws -> Double) rethrows -> (Double, Double) {
    let start = DispatchTime.now().uptimeNanoseconds
    let checksum = try autoreleasepool(invoking: work)
    let elapsed = Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000
    return (elapsed, checksum)
}

func sameItems(_ lhs: [SMSItem], _ rhs: [SMSItem]) -> Bool {
    guard lhs.count == rhs.count else { return false }
    return zip(lhs, rhs).allSatisfy { a, b in
        a.id == b.id && a.backendID == b.backendID && a.sender == b.sender &&
        a.content == b.content && a.code == b.code && a.timestamp == b.timestamp &&
        a.moduleStorage == b.moduleStorage && a.moduleIndex == b.moduleIndex &&
        a.archived == b.archived && a.direction == b.direction && a.device == b.device
    }
}

func decodingError(_ decode: () throws -> [SMSItem]) -> String? {
    do { _ = try decode(); return nil }
    catch let error as DecodingError {
        switch error {
        case .dataCorrupted(let context):
            return "corrupt|\(context.codingPath.map(\.stringValue))|\(context.debugDescription)"
        case .keyNotFound(let key, let context):
            return "missing|\(key.stringValue)|\(context.codingPath.map(\.stringValue))|\(context.debugDescription)"
        case .typeMismatch(let type, let context):
            return "type|\(type)|\(context.codingPath.map(\.stringValue))|\(context.debugDescription)"
        case .valueNotFound(let type, let context):
            return "value|\(type)|\(context.codingPath.map(\.stringValue))|\(context.debugDescription)"
        @unknown default:
            return String(describing: error)
        }
    } catch { return String(describing: error) }
}

@main
enum Benchmark {
    static func main() throws {
        let baseline = BaselineAPIJSONDecoder.makeDecoder()
        var correctnessCases = 0
        for format in validFormats + invalidFormats {
            let data = try fixture(1, formats: [format])
            let old = try? baseline.decode([SMSItem].self, from: data)
            let new = try? APIClient.decodeResponse([SMSItem].self, from: data)
            precondition((old == nil) == (new == nil), "Acceptance mismatch: \(format)")
            if let old, let new { precondition(sameItems(old, new), "Value mismatch: \(format)") }
            if invalidFormats.contains(format) { precondition(old == nil, "Invalid fixture accepted: \(format)") }
            correctnessCases += 1
        }
        for payload in ["[{\"timestamp\":null}]", "[{\"timestamp\":42}]", "[{}]", "[{\"timestamp\":true}]", "[", "{}"] {
            let data = Data(payload.utf8)
            let oldError = decodingError { try baseline.decode([SMSItem].self, from: data) }
            let newError = decodingError { try APIClient.decodeResponse([SMSItem].self, from: data) }
            precondition(oldError != nil && oldError == newError, "Structural error mismatch: \(payload)")
            correctnessCases += 1
        }
        for format in validFormats + invalidFormats {
            let data = try fixture(1, formats: [format])
            let oldError = decodingError { try baseline.decode([SMSItem].self, from: data) }
            let newError = decodingError { try APIClient.decodeResponse([SMSItem].self, from: data) }
            precondition(oldError == newError, "Date error mismatch: \(format)")
        }
        let equivalentZones = try APIClient.decodeResponse([SMSItem].self, from: fixture(3, formats: Array(validFormats[9...11])))
        precondition(equivalentZones.dropFirst().allSatisfy { $0.timestamp == equivalentZones[0].timestamp })
        let concurrentData = try fixture(100)
        let concurrentExpected = try baseline.decode([SMSItem].self, from: concurrentData)
        DispatchQueue.concurrentPerform(iterations: 32) { _ in
            autoreleasepool {
                let items = try! APIClient.decodeResponse([SMSItem].self, from: concurrentData)
                precondition(sameItems(items, concurrentExpected), "Concurrent response decode mismatch")
            }
        }

        let warmups = 6
        let samples = 40
        var results: [CaseResult] = []
        var checksum = 0.0
        for count in [0, 100, 1000] {
            let data = try fixture(count)
            let predecoded = try baseline.decode([SMSItem].self, from: data)
            let candidate = try APIClient.decodeResponse([SMSItem].self, from: data)
            precondition(sameItems(predecoded, candidate))
            let grouped = group(predecoded)
            let groupedCandidate = group(candidate)
            precondition(Set(grouped.map(\.phone)) == Set(groupedCandidate.map(\.phone)))
            for conversation in grouped {
                precondition(sameItems(conversation.messages, groupedCandidate.first(where: { $0.phone == conversation.phone })!.messages))
            }
            var timings: [[Double]] = Array(repeating: [], count: 5)
            for iteration in -warmups..<samples {
                // Rotate and reverse order to reduce systematic run-order bias.
                let shift = (iteration + warmups) % 5
                var order = (0..<5).map { ($0 + shift) % 5 }
                if iteration % 2 == 0 { order.reverse() }
                for operation in order {
                    let measured = try timed {
                        switch operation {
                        case 0:
                            let items = try baseline.decode([SMSItem].self, from: data)
                            return Double(items.count) + (items.last?.timestamp.timeIntervalSince1970 ?? 0)
                        case 1:
                            let items = try APIClient.decodeResponse([SMSItem].self, from: data)
                            return Double(items.count) + (items.last?.timestamp.timeIntervalSince1970 ?? 0)
                        case 2:
                            let conversations = group(predecoded)
                            return Double(conversations.reduce(0) { $0 + $1.messages.count })
                        case 3:
                            let conversations = group(try baseline.decode([SMSItem].self, from: data))
                            return Double(conversations.reduce(0) { $0 + $1.messages.count })
                        default:
                            let conversations = group(try APIClient.decodeResponse([SMSItem].self, from: data))
                            return Double(conversations.reduce(0) { $0 + $1.messages.count })
                        }
                    }
                    checksum += measured.1
                    if iteration >= 0 { timings[operation].append(measured.0) }
                }
            }
            let baselineStats = Stats(timings[0])
            let productionStats = Stats(timings[1])
            let saved = baselineStats.p95MS - productionStats.p95MS
            let percent = saved / baselineStats.p95MS * 100
            let result = CaseResult(
                fixtureCount: count, fixtureBytes: data.count,
                baselineDecode: baselineStats, productionDecodeIncludingSetup: productionStats,
                groupOnly: Stats(timings[2]), baselineDecodeAndGroup: Stats(timings[3]),
                productionDecodeAndGroupIncludingSetup: Stats(timings[4]),
                p95DecodeSavedMS: saved, p95DecodeSavedPercent: percent,
                passesGate: saved >= 50 && percent >= 25)
            results.append(result)
            print(String(format: "n=%d bytes=%d decode median/p95=%.3f/%.3f ms production=%.3f/%.3f ms grouping=%.3f/%.3f ms saved p95=%.3f ms (%.2f%%) gate=%@", count, data.count, baselineStats.medianMS, baselineStats.p95MS, productionStats.medianMS, productionStats.p95MS, result.groupOnly.medianMS, result.groupOnly.p95MS, saved, percent, result.passesGate.description))
        }
        let report = Report(
            generatedAt: ISO8601DateFormatter().string(from: Date()), warmups: warmups,
            samples: samples, ordering: "five operations rotate and reverse every sample",
            scope: "Offline synthetic JSON; frozen baseline decoder and actual production APIClient.decodeResponse and SMSModels; exact view grouping; excludes IPC, hardware, UI rendering, notifications and main-actor scheduling; production decoder/formatter setup included, baseline API/URLSession setup excluded",
            correctnessCases: correctnessCases, stressConcurrentDecodes: 32,
            cases: results, checksum: checksum)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "results.json"
        try encoder.encode(report).write(to: URL(fileURLWithPath: output))
        print("Saved raw timing samples to \(output); correctness=\(correctnessCases), concurrentDecodes=32, checksum=\(checksum)")
    }
}

