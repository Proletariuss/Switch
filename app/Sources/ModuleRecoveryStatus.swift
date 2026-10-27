import Combine
import Foundation

/// Module-only startup isolation reported by the Rust runtime. A historical
/// lock is evidence to inspect, never permission for the UI to remove it.
struct ModuleRecoveryStatus: Decodable, Equatable {
    let state: String
    let reasonCodes: [String]
    let historicalRecoveryConfirmed: Bool

    enum CodingKeys: String, CodingKey {
        case state
        case reasonCodes = "reason_codes"
        case historicalRecoveryConfirmed = "historical_recovery_confirmed"
    }

    private static let allowedReasons: Set<String> = [
        "legacy_execution_pending", "restore_journal_pending", "restore_journal_unreadable",
        "reconciliation_pending", "reconciliation_unreadable", "legacy_call_mode_lock",
        "legacy_adb_interface_lock", "legacy_lock_provenance_pending", "module_backup_present",
        "legacy_voice_runtime", "managed_voice_control_pending", "invalid_pristine_marker",
        "unreadable_evidence", "lifetime_authority_changed"
    ]

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        state = try values.decode(String.self, forKey: .state)
        historicalRecoveryConfirmed = try values.decode(Bool.self, forKey: .historicalRecoveryConfirmed)
        let reasons = try values.decode([String].self, forKey: .reasonCodes)
        guard state == "legacy_recovery_required", !historicalRecoveryConfirmed,
              !reasons.isEmpty, reasons.count <= 14,
              reasons.allSatisfy(Self.allowedReasons.contains) else {
            throw DecodingError.dataCorruptedError(
                forKey: .reasonCodes, in: values, debugDescription: "Invalid module recovery metadata")
        }
        var seen = Set<String>()
        reasonCodes = reasons.filter { seen.insert($0).inserted }
    }

    var isIsolated: Bool {
        state == "legacy_recovery_required" && !historicalRecoveryConfirmed
    }

    var reasonSummary: String {
        guard !reasonCodes.isEmpty else { return "未提供原因码" }
        return reasonCodes.joined(separator: " · ")
    }
}

struct ModuleRecoveryRuntimeResponse: Decodable {
    let moduleRecovery: ModuleRecoveryStatus?

    enum CodingKeys: String, CodingKey {
        case moduleRecovery = "module_recovery"
    }
}

protocol ModuleRecoveryStatusAPI {
    func read() async throws -> ModuleRecoveryRuntimeResponse
}

private struct LiveModuleRecoveryStatusAPI: ModuleRecoveryStatusAPI {
    func read() async throws -> ModuleRecoveryRuntimeResponse {
        try await APIClient(timeoutInterval: 5).get("api/v2/runtime")
    }
}

@MainActor final class ModuleRecoveryStatusStore: ObservableObject {
    enum ReadState: Equatable { case idle, loading, loaded, failed }

    @Published private(set) var snapshot: ModuleRecoveryStatus?
    @Published private(set) var readState: ReadState = .idle
    private let api: any ModuleRecoveryStatusAPI
    private var revision: UInt64 = 0

    init(api: (any ModuleRecoveryStatusAPI)? = nil) {
        self.api = api ?? LiveModuleRecoveryStatusAPI()
    }

    func refresh() async {
        guard !Task.isCancelled else { return }
        revision &+= 1
        let request = revision
        // A new read has no current observation until its response arrives.
        // Never present the previous backend's snapshot as current evidence.
        snapshot = nil
        readState = .loading
        do {
            let response = try await api.read()
            guard revision == request else { return }
            guard !Task.isCancelled else {
                readState = .failed
                return
            }
            snapshot = response.moduleRecovery
            readState = .loaded
        } catch {
            guard revision == request else { return }
            // Keep failures visible without exporting or displaying raw errors.
            // Cancellation alone also cannot prove that the module recovered.
            readState = .failed
        }
    }

    func invalidate() {
        revision &+= 1
        snapshot = nil
        readState = .idle
    }
}
