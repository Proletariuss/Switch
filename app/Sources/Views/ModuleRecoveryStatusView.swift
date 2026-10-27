import SwiftUI

/// A read-only explanation of why the Rust backend deliberately did not start
/// the module AT/QMI producers. It offers no recovery or lock-file action.
struct ModuleRecoveryStatusView: View {
    @EnvironmentObject private var backend: BackendProcess
    @StateObject private var store: ModuleRecoveryStatusStore
    @State private var request: Task<Void, Never>?
    let embedded: Bool
    let isolationReported: Bool

    @MainActor init(
        embedded: Bool = false,
        isolationReported: Bool = false,
        store: ModuleRecoveryStatusStore? = nil
    ) {
        self.embedded = embedded
        self.isolationReported = isolationReported
        _store = StateObject(wrappedValue: store ?? ModuleRecoveryStatusStore())
    }

    var body: some View {
        // The initial state has no snapshot. Keep a concrete mounted view so
        // the first read does not depend on an empty Group receiving onAppear.
        VStack(alignment: .leading, spacing: 0) {
            Color.clear.frame(height: 0).accessibilityHidden(true)
            if store.readState == .loading || store.readState == .failed
                || store.snapshot?.isIsolated == true || isolationReported {
                VStack(alignment: .leading, spacing: 7) {
                    if store.readState == .failed {
                        Label("模块恢复状态读取失败", systemImage: "exclamationmark.shield.fill")
                            .font(.callout.weight(.semibold))
                            .foregroundStyle(.orange)
                        Text("当前状态未知，不能据此判断模块已恢复。请重试读取状态。")
                            .foregroundStyle(.secondary)
                    } else if store.readState == .loading {
                        HStack(spacing: 7) {
                            ProgressView().controlSize(.small)
                            Text("正在读取模块恢复状态…")
                        }
                        Text("当前状态待确认；读取状态不会执行模块恢复。")
                            .foregroundStyle(.secondary)
                    } else if let status = store.snapshot, status.isIsolated {
                        Label("DJI 模块已隔离：旧版恢复待核对", systemImage: "exclamationmark.shield.fill")
                            .font(.callout.weight(.semibold))
                            .foregroundStyle(.orange)
                        Text("状态：\(status.state)")
                        Text("原因码：\(status.reasonSummary)")
                            .textSelection(.enabled)
                        Text("后端已暂停模块 AT/QMI 控制；USB 是否插入需另行核对。历史锁文件是恢复证据，请勿直接删除。读卡器功能以其自身状态为准。")
                            .foregroundStyle(.secondary)
                    } else {
                        Label("模块隔离详情待核对", systemImage: "exclamationmark.shield.fill")
                            .font(.callout.weight(.semibold))
                            .foregroundStyle(.orange)
                        Text("模块状态报告隔离，但尚未取得有效恢复原因。请重试读取状态。")
                            .foregroundStyle(.secondary)
                    }
                    Button(store.readState == .failed ? "重试读取状态" : "刷新隔离状态") { refresh() }
                        .controlSize(.small)
                        .disabled(backend.state != .running || store.readState == .loading)
                }
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
                .padding(embedded ? 0 : 12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .modifier(RecoveryCardStyle(embedded: embedded))
            }
        }
        .onAppear { refresh() }
        .onDisappear { cancel() }
        .compatibleOnChange(of: backend.state) { _ in refresh() }
        .compatibleOnChange(of: isolationReported) { _ in refresh() }
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

private struct RecoveryCardStyle: ViewModifier {
    let embedded: Bool
    @ViewBuilder func body(content: Content) -> some View {
        if embedded { content } else { content.workspaceCard() }
    }
}
