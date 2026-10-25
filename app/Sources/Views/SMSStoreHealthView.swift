import SwiftUI

/// A degraded store does not turn a reachable gateway into a failed process.
/// Query only when this diagnostic page opens, restarts, or is refreshed.
struct SMSStoreHealthView: View {
    @EnvironmentObject private var backend: BackendProcess
    @StateObject private var store = SMSStoreHealthStore()
    @State private var request: Task<Void, Never>?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Label("短信存储", systemImage: "internaldrive")
                    .font(.callout.weight(.semibold))
                if let snapshot = store.snapshot {
                    Text(snapshot.degraded ? "部分功能受限" : "正常")
                        .font(.caption)
                        .foregroundStyle(snapshot.degraded ? Color.orange : Color.secondary)
                }
                Spacer()
                if store.loading { ProgressView().controlSize(.small) }
                Button { refresh() } label: {
                    Label("查询状态", systemImage: "arrow.clockwise")
                }.controlSize(.small)
                    .disabled(store.loading || backend.state != .running)
            }
            if let snapshot = store.snapshot {
                Text("设备发件：\(snapshot.deviceSend.label) · N待存储：\(snapshot.nwait.label)")
                    .font(.caption).foregroundStyle(.secondary)
                if snapshot.degraded {
                    Text("相关短信操作已暂停，原记录保留。请先排查存储问题，再正常退出并重新打开应用进行校验；不要清空记录或反复发送。其他功能以各自状态为准。")
                        .font(.caption).foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            } else if backend.state != .running {
                Text("后端未运行，短信存储状态尚未确认。")
                    .font(.caption).foregroundStyle(.secondary)
            } else if let detail = store.detail {
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .workspaceCard()
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
