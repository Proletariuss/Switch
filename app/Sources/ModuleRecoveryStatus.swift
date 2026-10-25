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
    @Published private(set) var snapshot: ModuleRecoveryStatus?
    private let api: any ModuleRecoveryStatusAPI
    private var revision: UInt64 = 0

    init(api: (any ModuleRecoveryStatusAPI)? = nil) {
        self.api = api ?? LiveModuleRecoveryStatusAPI()
    }

    func refresh() async {
        revision &+= 1
        let request = revision
        snapshot = nil
        do {
            let response = try await api.read()
            guard !Task.isCancelled, revision == request else { return }
            snapshot = response.moduleRecovery
        } catch {
            // A failed diagnostic read must never preserve an old quarantine
            // projection as a statement about the current backend instance.
        }
    }

    func invalidate() {
        revision &+= 1
        snapshot = nil
    }
}
