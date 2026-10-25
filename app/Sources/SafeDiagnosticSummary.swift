import Foundation

/// Deliberate allowlist: runtime snapshots, free-form errors, network addresses,
/// reader identities, SMS and raw logs never enter this export model.
struct SafeDiagnosticSummary: Encodable {
    struct Exit: Encodable {
        let status: Int32
        let signaled: Bool
        let expected: Bool
    }
    struct Network: Encodable {
        let partial: Bool
        let durationMS: Int64?
        let completedCommands: Int
        let failedCommands: [String]
        let usbNetworkPresent: Bool?
        let interfaceCount: Int

        init(partial: Bool, durationMS: Int64?, completedCommands: Int, failedCommands: [String],
             usbNetworkPresent: Bool?, interfaceCount: Int) {
            let allowed: Set<String> = ["usbnet", "usbcfg", "cgdcont", "cgact", "cgpaddr"]
            self.partial = partial
            self.durationMS = durationMS
            self.completedCommands = min(5, max(0, completedCommands))
            self.failedCommands = failedCommands.filter { allowed.contains($0) }
            self.usbNetworkPresent = usbNetworkPresent
            self.interfaceCount = max(0, interfaceCount)
        }
    }
    let schemaVersion = 1
    let createdAt = Date()
    let version: String
    let build: String
    let backendState: String
    let lastExit: Exit?
    let moduleSnapshotStale: Bool
    let readerConnected: Bool
    let moduleSMSReady: Bool
    let readerSMSReady: Bool
    let collectedAt: Date?
    let network: Network?

    func data() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        return try encoder.encode(self)
    }
}
