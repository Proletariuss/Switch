import SwiftUI

/// Pending SGP.22 notifications waiting for operator delivery/confirmation.
/// The toolbar follows the project's fixed icon-only, no-background style.
struct ReaderESIMNotificationsView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.workspaceViewport) private var viewport
    @ObservedObject var store: ReaderESIMStore
    let privacyMaskEnabled: Bool

    var body: some View {
        VStack(spacing: viewport.sectionSpacing) {
            HStack(spacing: 14) {
                WorkspaceSymbol(systemName: "bell.badge", tint: WorkspaceColors.accent, size: viewport.isCompactHeight ? 36 : 46)
                VStack(alignment: .leading, spacing: 5) {
                    Text("运营商通知")
                        .font(.system(size: viewport.isCompactHeight ? 19 : 22, weight: .bold))
                    Text("完成 Profile 操作与运营商服务器的状态同步")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    store.loadNotifications()
                } label: {
                    if store.isLoadingNotifications {
                        ProgressView()
                            .controlSize(.small)
                            .frame(width: 28, height: 28)
                    } else {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 14, weight: .semibold))
                            .frame(width: 28, height: 28)
                            .contentShape(Rectangle())
                    }
                }
                .buttonStyle(.plain)
                .foregroundStyle(WorkspaceColors.accent)
                .disabled(store.isLoadingNotifications || store.retryingNotificationID != nil)
                .help("重新读取并补发待处理通知")

                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 13, weight: .semibold))
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("关闭")
            }
            if !store.notifications.isEmpty {
                HStack {
                    Text("待确认通知")
                        .font(.callout.weight(.medium))
                    Spacer()
                    Text("\(store.notifications.count) 条")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(WorkspaceColors.accent)
                }
            }

            Group {
                if store.isLoadingNotifications && store.notifications.isEmpty {
                    ProgressView("正在检查 eUICC 通知…")
                        .controlSize(.small)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let error = store.notificationError, store.notifications.isEmpty {
                    ContentUnavailableView(
                        "通知读取失败",
                        systemImage: "exclamationmark.triangle",
                        description: Text(error))
                } else if store.notifications.isEmpty {
                    ContentUnavailableView(
                        "通知已全部确认",
                        systemImage: "checkmark.seal",
                        description: Text("当前没有等待发送给运营商的 Profile 通知"))
                } else {
                    ScrollView {
                        LazyVStack(spacing: viewport.isCompactHeight ? 7 : 10) {
                            ForEach(store.notifications) { item in
                                notificationRow(item)
                            }
                        }
                        .padding(.bottom, 2)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            if let error = store.notificationError, !store.notifications.isEmpty {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
                    .background(.red.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
            }
        }
        .padding(viewport.pageInset)
        .frame(width: min(650, max(520, viewport.width - 44)), height: min(520, max(330, viewport.height - 44)))
        .environment(\.workspaceAccent, WorkspaceColors.accent)
        .environment(\.workspaceVisualStyle, .nekoko)
        .proletariusPageBackground()
        .onAppear {
            if store.notifications.isEmpty && !store.isLoadingNotifications {
                store.loadNotifications()
            }
        }
    }

    private func notificationRow(_ item: ESIMNotificationItem) -> some View {
        HStack(spacing: 12) {
            WorkspaceSymbol(systemName: eventIcon(item.event), tint: WorkspaceColors.accent, size: 36)

            VStack(alignment: .leading, spacing: 4) {
                Text(eventTitle(item.event))
                    .font(.subheadline.weight(.semibold))
                HStack(spacing: 8) {
                    Text("序号 \(item.sequenceNumber)")
                    if let iccid = item.iccid, !iccid.isEmpty {
                        Text("ICCID \(PrivacyMask.value(iccid, enabled: privacyMaskEnabled))")
                            .copyableInformation(iccid, enabled: !privacyMaskEnabled)
                    }
                }
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                if let address = item.address, !address.isEmpty {
                    Text(address)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .copyableInformation(address)
                }
            }

            Spacer()

            Button {
                store.retryNotification(item)
            } label: {
                if store.retryingNotificationID == item.id {
                    ProgressView().controlSize(.small)
                } else {
                    Label("发送", systemImage: "paperplane")
                        .font(.caption.weight(.semibold))
                }
            }
            .buttonStyle(.proletariusPrimary)
            .controlSize(.small)
            .disabled(!item.canRetry || store.retryingNotificationID != nil)
        }
        .padding(viewport.isCompactHeight ? 12 : 16)
        .workspaceCard()
    }

    private func eventTitle(_ event: String) -> String {
        switch event {
        case "install": return "下载与安装通知"
        case "enable": return "启用与切卡通知"
        case "disable": return "停用通知"
        case "delete": return "删除通知"
        default: return "Profile 状态通知"
        }
    }

    private func eventIcon(_ event: String) -> String {
        switch event {
        case "install": return "arrow.down"
        case "enable": return "checkmark"
        case "disable": return "pause"
        case "delete": return "trash"
        default: return "bell"
        }
    }
}
