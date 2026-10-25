import SwiftUI

/// 通话记录弹窗
struct CallHistoryView: View {
    @EnvironmentObject private var store: DashboardStore
    @EnvironmentObject private var attentionStore: AttentionStore
    @Environment(\.dismiss) private var dismiss
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    /// 待确认删除的单条记录
    @State private var pendingDelete: CallRecord?
    /// 是否确认清空全部
    @State private var confirmClear = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "clock.arrow.circlepath", tint: WorkspaceColors.accent, size: 36)
                WorkspaceSectionHeader(title: "通话记录", detail: "\(store.callHistory.count) 次呼叫")
                Spacer()
                if !store.callHistory.isEmpty {
                    Button("清空", role: .destructive) {
                        confirmClear = true
                    }
                    .controlSize(.small)
                }
                Button("关闭") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .controlSize(.small)
            }
            .padding(20)

            Divider()

            if store.callHistory.isEmpty {
                VStack(spacing: 8) {
                    WorkspaceSymbol(systemName: "phone", tint: WorkspaceColors.accent, size: 52)
                    Text("暂无通话记录")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(store.callHistory) { record in
                    CallHistoryRow(record: record) {
                        pendingDelete = record
                    }
                }
                .listStyle(.inset)
                .scrollContentBackground(.hidden)
                .padding(.horizontal, 8)
            }
        }
        .frame(width: 560, height: 480)
        .background(WorkspaceColors.surface)
        .buttonStyle(.proletarius)
        .environment(\.workspaceAccent, WorkspaceColors.accent)
        .confirmationDialog(
            "删除这条通话记录？",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }),
            presenting: pendingDelete
        ) { record in
            Button("删除", role: .destructive) {
                store.deleteCallRecord(record.id)
            }
            Button("取消", role: .cancel) {}
        } message: { record in
            Text("号码：\(PrivacyMask.value(record.number ?? "未知号码", enabled: privacyMaskEnabled))")
        }
        .confirmationDialog("清空全部通话记录？", isPresented: $confirmClear) {
            Button("清空", role: .destructive) {
                store.clearCallHistory()
            }
            Button("取消", role: .cancel) {}
        } message: {
            Text("将删除全部 \(store.callHistory.count) 条通话记录，此操作不可恢复。")
        }
        .onAppear {
            attentionStore.setViewingCalls(true)
            store.loadCallHistory()
        }
        .onDisappear {
            attentionStore.setViewingCalls(false)
        }
    }
}

/// 单条通话记录
struct CallHistoryRow: View {
    let record: CallRecord
    var onDelete: () -> Void
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: iconName)
                .foregroundStyle(iconColor)
                .frame(width: 28, height: 34)

            VStack(alignment: .leading, spacing: 2) {
                Text(PrivacyMask.value(record.number ?? "未知号码", enabled: privacyMaskEnabled))
                    .font(.system(size: 13, weight: .semibold))
                    .copyableInformation(record.number ?? "", enabled: !privacyMaskEnabled)
                Text("\(record.deviceName) · \(record.startedAt.formatted(date: .abbreviated, time: .standard))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            if record.isIncoming {
                Text(record.isMissed ? "未接" : "已接")
                    .font(.caption.bold())
                    .foregroundStyle(record.isMissed ? Color.red : Color.green)
            }

            Text(durationText)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)

            Button {
                onDelete()
            } label: {
                Image(systemName: "trash")
                    .font(.caption)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("删除这条记录")
        }
        .padding(.vertical, 8)
    }

    private var iconName: String {
        if record.isMissed { return "phone.arrow.down.left.fill" }
        return record.isIncoming ? "phone.arrow.down.left" : "phone.arrow.up.right"
    }

    private var iconColor: Color {
        if record.isMissed { return .red }
        return record.isIncoming ? .green : WorkspaceColors.accent
    }

    private var durationText: String {
        guard record.duration > 0 else { return "-" }
        let minutes = record.duration / 60
        let seconds = record.duration % 60
        return minutes > 0 ? "\(minutes)分\(seconds)秒" : "\(seconds)秒"
    }
}
