import Combine
import Foundation

protocol DiagnosticEventAPI {
    func snapshot() async throws -> DiagnosticEventSnapshot
}
private struct LiveDiagnosticEventAPI: DiagnosticEventAPI {
    func snapshot() async throws -> DiagnosticEventSnapshot {
        try await APIClient(timeoutInterval: 10).get("api/diagnostics/events")
    }
}

@MainActor final class DiagnosticEventStore: ObservableObject {
    @Published private(set) var snapshot: DiagnosticEventSnapshot?
    @Published private(set) var loading = false
    @Published private(set) var error: String?
    private let api: any DiagnosticEventAPI
    private var requestRevision: UInt64 = 0
    init(api: (any DiagnosticEventAPI)? = nil) { self.api = api ?? LiveDiagnosticEventAPI() }

    func refresh() async {
        requestRevision &+= 1
        let request = requestRevision
        loading = true
        error = nil
        defer { if request == requestRevision { loading = false } }
        do {
            let value = try await api.snapshot()
            guard !Task.isCancelled, request == requestRevision else { return }
            snapshot = value
        } catch {
            guard !Task.isCancelled, request == requestRevision else { return }
            self.error = "安全事件读取未完成，请稍后重试。"
        }
    }
    func invalidate() {
        requestRevision &+= 1
        loading = false
    }
}
