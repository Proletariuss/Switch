import AppKit
import SwiftUI

/// Menu bar label and dashboard presentation. This file owns presentation-only
/// state; device, reader, IMS and traffic data remain in their domain stores.
struct MenuBarStatusLabel: View {
    @Environment(\.openWindow) private var openWindow
    @ObservedObject var appDelegate: AppDelegate
    @ObservedObject private var mainWindowRequests = MainWindowRequestCenter.shared
    @ObservedObject var attentionStore: AttentionStore
    @State private var handledWindowRequest = 0
    let store: DashboardStore

    private var presentation: MenuBarPresentation {
        appDelegate.menuBarPresentation
    }

    private var menuBarBadgeImage: NSImage? {
        guard attentionStore.hasAttention else { return nil }
        return makeMenuBarAttentionBadgeImage(count: attentionStore.totalCount)
    }

    private var accessibilitySummary: String {
        [
            presentation.accessibilitySummary,
            attentionStore.accessibilitySummary,
        ]
            .compactMap { $0 }
            .joined(separator: "；")
    }

    var body: some View {
        HStack(alignment: .center, spacing: 3.5) {
            if let image = presentation.image {
                Image(nsImage: image)
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 15, height: 14, alignment: .center)
            }
            if let title = presentation.rateTitles.combined {
                Text(title)
                    .font(.caption2)
                    .monospacedDigit()
            }
            if let badgeImage = menuBarBadgeImage {
                Image(nsImage: badgeImage)
                    .renderingMode(.template)
                    .resizable()
                    .interpolation(.high)
                    .frame(width: badgeImage.size.width, height: badgeImage.size.height)
            }
        }
        .fixedSize()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilitySummary)
        .help(accessibilitySummary)
        .onAppear {
            appDelegate.bindDashboardStore(store)
            handleMainWindowRequest()
        }
        .compatibleOnChange(of: mainWindowRequests.generation) { _ in
            handleMainWindowRequest()
        }
    }

    private func handleMainWindowRequest() {
        guard handledWindowRequest != mainWindowRequests.generation else { return }
        handledWindowRequest = mainWindowRequests.generation
        NSApp.activate(ignoringOtherApps: true)
        openWindow(id: AppSceneID.mainWindow)
    }
}

/// 菜单栏标签只保留一个动态 SwiftUI Text；徽标文字预先绘制进 NSImage，
/// 避免 MenuBarExtra 对多个动态 Text 子节点的运行时限制。
@MainActor
private func makeMenuBarAttentionBadgeImage(count: Int) -> NSImage {
    let title = AttentionStore.compactCount(count) as NSString
    let font = NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .semibold)
    let attributes: [NSAttributedString.Key: Any] = [
        .font: font,
        .foregroundColor: NSColor.black,
    ]
    let textSize = title.size(withAttributes: attributes)
    let height: CGFloat = 14
    let width = ceil(textSize.width) + 2
    let image = NSImage(size: NSSize(width: width, height: height))
    image.lockFocus()
    title.draw(
        at: NSPoint(
            x: floor((width - textSize.width) / 2),
            y: floor((height - textSize.height) / 2)),
        withAttributes: attributes)
    image.unlockFocus()
    image.isTemplate = true
    image.accessibilityDescription = "待处理提醒 \(count) 个"
    return image
}

struct MenuBarDashboardPanel: View {
    @ObservedObject var appDelegate: AppDelegate
    @ObservedObject var backend: BackendProcess
    @ObservedObject var store: DashboardStore
    @ObservedObject var smsStore: SMSStore
    @ObservedObject var attentionStore: AttentionStore
    @ObservedObject var updateChecker: UpdateChecker

    @AppStorage(MenuBarDisplayOptions.showSignalKey) private var menuBarShowSignal = false
    @AppStorage(MenuBarDisplayOptions.showDownloadKey) private var menuBarShowDownload = false
    @AppStorage(MenuBarDisplayOptions.showUploadKey) private var menuBarShowUpload = false
    @AppStorage("silentLaunch") private var silentLaunchEnabled = false
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    @State private var autoLaunchEnabled = false

    private var presentation: MenuBarPresentation {
        appDelegate.menuBarPresentation
    }

    var body: some View {
        VStack(spacing: 8) {
            header
            VStack(alignment: .leading, spacing: 0) {
                sectionHeading("设备与 VoWiFi")
                deviceStatusSection
            }
            .padding(.horizontal, 12)
            .padding(.top, 10)
            .workspaceCard()

            VStack(alignment: .leading, spacing: 0) {
                sectionHeading("通信")
                communicationStatusSection
            }
            .padding(.horizontal, 12)
            .padding(.top, 10)
            .workspaceCard()

            VStack(alignment: .leading, spacing: 0) {
                trafficSection
                updateSection
            }
            .padding(.horizontal, 12)
            .workspaceCard()

            Divider()

            footer
        }
        .padding(12)
        .frame(width: 320)
        .background(WorkspaceColors.canvas)
        .environment(\.workspaceVisualStyle, .nekoko)
        .tint(WorkspaceColors.accent)
        .onAppear {
            appDelegate.bindDashboardStore(store)
            autoLaunchEnabled = AutoLaunch.isEnabled
        }
        .onReceive(NotificationCenter.default.publisher(for: AutoLaunch.didChangeNotification)) { _ in
            autoLaunchEnabled = AutoLaunch.isEnabled
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .center, spacing: 6) {
                Text("Switch")
                    .font(.headline.weight(.semibold))
                    .lineLimit(1)

                Spacer(minLength: 8)

                HStack(spacing: 5) {
                    Circle()
                        .fill(connectionTint)
                        .frame(width: 6, height: 6)
                    Text(connectionStatusText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .fixedSize()
                .accessibilityElement(children: .combine)

                Button {
                    store.refresh()
                } label: {
                    Image(systemName: store.networkRecovering ? "arrow.trianglehead.2.clockwise.rotate.90" : "arrow.clockwise")
                        .frame(width: 22, height: 22)
                }
                .buttonStyle(.plain)
                .foregroundStyle(ProletariusPalette.action)
                .keyboardShortcut("r", modifiers: .command)
                .disabled(backend.state != .running || store.networkRecovering)
                .help("立即刷新（⌘R）")
                .accessibilityLabel(store.networkRecovering ? "正在恢复网络" : "立即刷新")
            }

            if let connectionDetailText {
                Text(connectionDetailText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 5)
        .help("\(connectionStatusText) · \(lastUpdatedCompactText)")
    }

    private func sectionHeading(_ title: String) -> some View {
        Text(title)
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(.secondary)
    }

    private var deviceStatusSection: some View {
        VStack(spacing: 8) {
            deviceButton(
                destination: .home, title: "模块", status: moduleStatusText,
                detail: moduleConnected ? joinedStatusParts([moduleOperatorText, moduleDetailText]) : nil,
                ims: moduleIMSShort, color: moduleStatusText == "已连接" ? ProletariusPalette.module : .secondary,
                timestamp: lastUpdatedCompactText)
            Divider().opacity(0.6)
            deviceButton(
                destination: .reader, title: "读卡器", status: readerStatusText,
                detail: backend.state == .running && store.readerConnected ? joinedStatusParts([readerOperatorText, readerDetailText]) : nil,
                ims: readerIMSShort, color: backend.state == .running && store.readerConnected ? ProletariusPalette.reader : .secondary)
        }
        .padding(.vertical, 10)
    }

    private func deviceButton(destination: AppSection, title: String, status: String,
                              detail: String?, ims: String, color: Color,
                              timestamp: String? = nil) -> some View {
        Button {
            navigate(to: destination)
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                compactStatusRow(title: title, value: status, connected: status == "已连接", color: color)
                if let detail {
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                HStack {
                    Text("VoWiFi · \(ims)")
                        .foregroundStyle(ims == "已就绪" ? Color.green : ims == "异常" ? Color.orange : Color.secondary)
                    Spacer(minLength: 4)
                    if let timestamp { Text(timestamp).foregroundStyle(.secondary) }
                }
                .font(.system(size: 10))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help([title, status, detail, "VoWiFi \(ims)", timestamp, "点击打开页面"].compactMap { $0 }.joined(separator: " · "))
    }

    private var communicationStatusSection: some View {
        VStack(spacing: 10) {
            navigationRow(to: .sms, title: "短信中心", value: "\(smsStore.items.count) 条 · 未读 \(attentionStore.unreadSMSCount)",
                          color: attentionStore.unreadSMSCount > 0 ? .orange : .secondary)
            navigationRow(to: .calls, title: "通话", value: "\(callStatusText) · 未查看 \(attentionStore.unviewedCallCount)", color: callStatusColor)
        }
        .padding(.vertical, 10)
    }

    private func navigationRow(to destination: AppSection, title: String, value: String, color: Color) -> some View {
        Button { navigate(to: destination) } label: {
            compactStatusRow(title: title, value: value, connected: true, color: color)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("打开\(title)页面")
    }

    private func navigate(to destination: AppSection) {
        MainWindowRequestCenter.shared.requestOpen(destination: destination)
    }

    private func compactStatusRow(
        title: String,
        value: String,
        connected: Bool,
        color: Color
    ) -> some View {
        HStack(spacing: 7) {
            Circle().fill(connected ? color : Color.secondary.opacity(0.5)).frame(width: 5, height: 5)
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(width: 48, alignment: .leading)
            Text(value)
                .font(.caption)
                .foregroundStyle(.primary)
                .lineLimit(1)
                .truncationMode(.middle)
                .minimumScaleFactor(0.75)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }

    private var trafficSection: some View {
        VStack(spacing: 4) {
            HStack(spacing: 8) {
                Text("流量")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 4)
                Text("↓ \(displayedDownloadRate)")
                Text("↑ \(displayedUploadRate)")
            }
            .font(.caption2.monospacedDigit())
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .foregroundStyle(.secondary)

            Text(sessionSummaryText)
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                .minimumScaleFactor(0.75)
        }
        .padding(.vertical, 7)
    }

    @ViewBuilder
    private var updateSection: some View {
        if let release = updateChecker.pendingUpdate {
            Divider()

            HStack(spacing: 8) {
                Text("新版本 \(release.tagName) 可用")
                    .font(.callout)
                    .lineLimit(1)

                Spacer(minLength: 8)

                Button("下载") {
                    if let url = updateChecker.downloadURL {
                        NSWorkspace.shared.open(url)
                    }
                    updateChecker.dismissUpdate()
                }
                .controlSize(.small)
                .disabled(updateChecker.downloadURL == nil)
            }
            .padding(.vertical, 8)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            Button {
                showMainWindow()
            } label: {
                Text("打开主界面")
                    .foregroundStyle(.primary)
            }
            .buttonStyle(.plain)

            Spacer(minLength: 8)

            Menu {
                Toggle("登录时启动", isOn: autoLaunchBinding)
                Toggle("静默启动", isOn: $silentLaunchEnabled)

                Divider()

                Toggle("菜单栏显示信号", isOn: signalDisplayBinding)
                Toggle("菜单栏显示下载速率", isOn: downloadDisplayBinding)
                Toggle("菜单栏显示上传速率", isOn: uploadDisplayBinding)

                Divider()

                Button("通知设置…") {
                    openNotificationSettings()
                }
            } label: {
                Text("设置")
                    .foregroundStyle(.secondary)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help("启动方式、菜单栏显示与系统通知设置")

            Button {
                NSApp.terminate(nil)
            } label: {
                Text("退出")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .keyboardShortcut("q", modifiers: .command)
        }
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }


    private var moduleConnected: Bool {
        guard backend.state == .running, let status = store.status else { return false }
        if let hardwareStatus = trimmed(status.hardwareStatus), !hardwareStatus.isEmpty {
            return false
        }
        return MenuBarStatusPolicy.moduleHasControlResponse(
            imei: status.imei, iccid: status.iccid, imsi: status.imsi,
            operatorName: status.operatorName, networkMode: status.networkMode,
            simInserted: status.simInserted, signalDbm: status.signalDbm,
            registrationStatus: status.regStatus)
    }

    private var moduleStatusText: String {
        MenuBarStatusPolicy.moduleStatus(
            serviceRunning: backend.state == .running, hasStatus: store.status != nil,
            stale: store.statusStale, hardwareIssue: trimmed(store.status?.hardwareStatus) != nil,
            hardwarePresent: moduleConnected,
            enumeratedDevicePresent: store.status?.usbDevice != nil,
            simInserted: store.status?.simInserted,
            qmiSessionInitialized: store.status?.qmi?.sessionInitialized)
    }

    private var readerStatusText: String {
        guard backend.state == .running else { return "状态未知" }
        return store.readerConnected ? "已连接" : "未连接"
    }

    private var moduleOperatorText: String {
        guard moduleConnected else { return "未连接" }
        return trimmed(store.status?.operatorName) ?? "已连接"
    }

    private var moduleDetailText: String? {
        guard moduleConnected, let status = store.status else { return nil }
        let phone = trimmed(status.phoneNumber).map {
            PrivacyMask.value($0, enabled: privacyMaskEnabled)
        }
        let signal = status.signalDbm.map { "\($0) dBm" }
        return joinedStatusParts([phone, trimmed(status.networkMode), signal, trimmed(status.radioBand)])
    }

    private var readerOperatorText: String {
        guard store.readerConnected else { return "未连接" }
        guard let usim = store.readerUSIMInfo else { return "正在读取" }
        let flag = iccidFlagEmoji(usim.iccid) ?? mccFlagEmoji(usim.mcc) ?? ""
        return "\(flag)\(store.readerOperatorDisplayName)"
    }

    private var readerDetailText: String? {
        guard store.readerConnected else { return nil }
        let phone = store.readerUSIMInfo?.phoneNumber.map {
            PrivacyMask.value($0, enabled: privacyMaskEnabled)
        }
        return joinedStatusParts([phone, store.readerCardType == "euicc" ? "eUICC" : nil])
    }

    private var moduleIMSShort: String {
        guard backend.state == .running else { return "状态未知" }
        guard !store.statusStale else { return "数据过期" }
        guard moduleConnected else { return "未连接" }
        let status = store.voWiFiStatus
        return MenuBarStatusPolicy.imsStatus(
            loaded: store.voWiFiLoaded, failed: status?.state == "failed" || status?.state == "unavailable",
            registered: status?.vowifiActive == true && status?.imsRegistered == true,
            smsReady: status?.smsReady, callReady: status?.callReady,
            connecting: status?.vowifiActive == true || store.voWiFiActionInFlight)
    }

    private var readerIMSShort: String {
        guard backend.state == .running else { return "状态未知" }
        guard store.readerConnected else { return "未连接" }
        let status = store.readerVoWiFiStatus
        return MenuBarStatusPolicy.imsStatus(
            loaded: store.readerVoWiFiLoaded, failed: status?.imsState == "failed" || status?.imsState == "error",
            registered: status?.imsState == "registered", smsReady: status?.smsReady, callReady: status?.callReady,
            connecting: status?.imsState == "registering" || status?.tunnelState == "connecting")
    }

    private var callStatusColor: Color {
        guard backend.state == .running else { return .secondary }
        switch store.callStatus.state {
        case "incoming": return .orange
        case "active": return .green
        case "dialing", "alerting": return .blue
        default: return .secondary
        }
    }

    private func trimmed(_ value: String?) -> String? {
        guard let value else { return nil }
        let result = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return result.isEmpty ? nil : result
    }

    private func joinedStatusParts(_ values: [String?]) -> String? {
        let parts = values.compactMap { trimmed($0) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private var connectionStatusText: String {
        switch backend.state {
        case .running: return "服务运行中"
        case .starting: return "服务启动中"
        case .stopping: return "正在安全退出"
        case .stopped: return "服务已停止"
        case .failed: return "服务异常"
        }
    }

    private var connectionTint: Color {
        switch backend.state {
        case .running: return .green
        case .starting, .stopping:
            return .orange
        case .stopped:
            return Color(nsColor: .secondaryLabelColor)
        case .failed:
            return .red
        }
    }

    private var autoLaunchBinding: Binding<Bool> {
        Binding(
            get: { autoLaunchEnabled },
            set: { setAutoLaunch($0) })
    }

    private var signalDisplayBinding: Binding<Bool> {
        Binding(
            get: { menuBarShowSignal },
            set: {
                menuBarShowSignal = $0
                notifyMenuBarDisplayOptionsChanged()
            })
    }

    private var downloadDisplayBinding: Binding<Bool> {
        Binding(
            get: { menuBarShowDownload },
            set: {
                menuBarShowDownload = $0
                notifyMenuBarDisplayOptionsChanged()
            })
    }

    private var uploadDisplayBinding: Binding<Bool> {
        Binding(
            get: { menuBarShowUpload },
            set: {
                menuBarShowUpload = $0
                notifyMenuBarDisplayOptionsChanged()
            })
    }

    private var connectionDetailText: String? {
        if case .failed(let reason) = backend.state, !reason.isEmpty {
            return formatMenuText(reason)
        }
        if store.statusStale {
            return "模块数据过期；读卡器状态独立显示。"
        }
        if store.status?.qmi?.sessionInitialized == true {
            return "QMI 会话已完成初始化；当前链路、SIM、注册与网络状态尚未实时确认。"
        }
        let qmiError = store.status?.qmi?.initializationError?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if let hardwareStatus = store.status?.hardwareStatus,
           !hardwareStatus.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let detail = qmiError.flatMap { $0.isEmpty ? nil : "\(hardwareStatus) · QMI: \($0)" }
                ?? hardwareStatus
            return formatMenuText(detail)
        }
        if let qmiError, !qmiError.isEmpty {
            return formatMenuText("QMI: \(qmiError)")
        }
        return nil
    }

    private var sessionSummaryText: String {
        let download = trafficMetric(store.traffic?.sessionRX)
        let upload = trafficMetric(store.traffic?.sessionTX)
        let scope = store.statusStale || backend.state != .running ? "上次会话" : "本次"
        return "\(scope)  下载 \(download) · 上传 \(upload)"
    }

    private var displayedDownloadRate: String {
        backend.state == .running && !store.statusStale ? presentation.downloadRate : "—"
    }

    private var displayedUploadRate: String {
        backend.state == .running && !store.statusStale ? presentation.uploadRate : "—"
    }

    private var callStatusText: String {
        MenuBarStatusPolicy.callStatus(serviceRunning: backend.state == .running, state: store.callStatus.state)
    }

    private var lastUpdatedCompactText: String {
        guard let lastUpdated = store.lastUpdated else { return "等待更新" }
        return "更新 \(lastUpdated.formatted(date: .omitted, time: .standard))"
    }


    private func trafficMetric(_ bytes: UInt64?) -> String {
        guard store.traffic?.available == true, let bytes else { return "—" }
        let formatter = ByteCountFormatter()
        formatter.countStyle = .binary
        formatter.allowedUnits = [.useKB, .useMB, .useGB, .useTB]
        return formatter.string(fromByteCount: Int64(clamping: bytes))
    }

    private func formatMenuText(_ text: String, limit: Int = 48) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count > limit else { return trimmed }
        return String(trimmed.prefix(limit)) + "…"
    }

    private func setAutoLaunch(_ enabled: Bool) {
        do {
            try AutoLaunch.setEnabled(enabled)
            autoLaunchEnabled = enabled
        } catch {
            autoLaunchEnabled = AutoLaunch.isEnabled
            showSystemError(
                title: "无法修改开机自启",
                message: error.localizedDescription)
        }
    }

    private func showSystemError(title: String, message: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: "好")
        alert.runModal()
    }

    private func notifyMenuBarDisplayOptionsChanged() {
        NotificationCenter.default.post(
            name: MenuBarDisplayOptions.didChangeNotification,
            object: nil)
    }

    private func showMainWindow() {
        appDelegate.showMainWindow()
    }

    private func openNotificationSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }
}
