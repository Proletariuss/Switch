import SwiftUI

/// A read-only explanation of why the Rust backend deliberately did not start
/// the module AT/QMI producers. It offers no recovery or lock-file action.
struct ModuleRecoveryStatusView: View {
    @EnvironmentObject private var backend: BackendProcess
    @StateObject private var store = ModuleRecoveryStatusStore()
    @State private var request: Task<Void, Never>?

    var body: some View {
        Group {
            if let status = store.snapshot, status.isIsolated {
                VStack(alignment: .leading, spacing: 7) {
                    Label("DJI 模块已隔离：旧版恢复待核对", systemImage: "exclamationmark.shield.fill")
                        .font(.callout.weight(.semibold))
                        .foregroundStyle(.orange)
                    Text("状态：\(status.state)")
                    Text("原因码：\(status.reasonSummary)")
                        .textSelection(.enabled)
                    Text("后端已暂停模块 AT/QMI 控制；USB 是否插入需另行核对。历史锁文件是恢复证据，请勿直接删除。读卡器功能以其自身状态为准。")
                        .foregroundStyle(.secondary)
                    Button("刷新隔离状态") { refresh() }
                        .controlSize(.small)
                        .disabled(backend.state != .running)
                }
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .workspaceCard()
            }
        }
        .onAppear { refresh() }
        .onDisappear { cancel() }
        .compatibleOnChange(of: backend.state) { _ in refresh() }
    }

    private func refresh() {
        cancel()
        guard backend.state == .running else { return }
        request = Task { @MainActor in await store.refresh() }
    }

    private func cancel() {
        request?.cancel()
        request = nil
        store.invalidate()
    }
}
