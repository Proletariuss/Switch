import Foundation
import SwiftUI

/// 读取优先的活动页：连接概览与业务详情分层展示。
struct ActivityView: View {
    @StateObject private var readerService = BluetoothReaderService()
    @EnvironmentObject private var backend: BackendProcess
    @EnvironmentObject private var store: DashboardStore
    @EnvironmentObject private var smsStore: SMSStore
    @EnvironmentObject private var attentionStore: AttentionStore
    @Environment(\.workspaceViewport) private var viewport

    private var columns: [GridItem] {
        let gap = viewport.sectionSpacing
        let width = min(viewport.contentWidth, 1160 - viewport.pageInset * 2)
        let count = width >= 4 * 240 + gap * 3 ? 4 : (width >= 2 * 250 + gap ? 2 : 1)
        return Array(repeating: GridItem(.flexible(), spacing: gap, alignment: .top), count: count)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                overviewHeading

                statusStrip

                WorkspaceSectionHeader(title: "设备与业务", detail: "各设备的连接与业务状态独立更新。")

                LazyVGrid(columns: columns, alignment: .leading, spacing: viewport.sectionSpacing) {
                    deviceActivityCard
                    voWiFiActivityCard
                    communicationActivityCard
                    trafficActivityCard
                }

                if let error = store.voiceError ?? store.audioError ?? store.readerError {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                        VStack(alignment: .leading, spacing: 3) {
                            Text("需要关注").font(.headline)
                            Text(error).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
                        }
                    }
                    .padding(viewport.cardPadding)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .workspaceCard(tint: .orange)
                }
            }
            .padding(viewport.pageInset)
            .frame(maxWidth: 1160)
            .frame(maxWidth: .infinity)
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { store.refresh() } label: { Label("刷新", systemImage: "arrow.clockwise") }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.proletariusToolbar)
                    .foregroundStyle(ProletariusPalette.action)
                    .help("刷新设备状态")
                    .keyboardShortcut("r", modifiers: .command)
            }
            .compatibleSharedToolbarBackgroundHidden()
        }
    }

    @ViewBuilder
    private var overviewHeading: some View {
        if viewport.isCompactHeight {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "waveform.path.ecg", tint: WorkspaceColors.accent,
                                size: WorkspaceHeaderMetrics.iconFrame)
                VStack(alignment: .leading, spacing: 4) {
                    Text("OVERVIEW · 活动").font(.system(size: 18, weight: .semibold))
                    Text("连接状态、通信动态与网络用量。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
        } else {
            WorkspaceHero(title: "活动", subtitle: "连接状态、通信动态与网络用量。", symbol: "waveform.path.ecg", eyebrow: "OVERVIEW")
        }
    }

    private var statusStrip: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: viewport.isCompactHeight ? 100 : 140), spacing: 10)], alignment: .leading, spacing: 8) {
            ForEach(Array(statusItems.enumerated()), id: \.offset) { entry in
                let item = entry.element
                statusColumn(item.0, item.1, item.2)
            }
        }
        .padding(viewport.cardPadding)
        .workspaceCard()
        // DashboardStore forwards each child store's change notifications.
        // Preserve view identity across polls so copy feedback and accessibility
        // focus are not recreated when unrelated device state advances.
    }

    /// Compact overview only; the cards below own detailed device and IMS
    /// information, so this strip does not repeat every sub-state.
    private var statusItems: [(String, String, Color)] {
        let items: [(String, String, Color)] = [
            ("设备", "\(connectedDeviceCount) / 2 在线", deviceSummaryColor),
            ("后台", backendSummary, backendColor),
            ("VoWiFi", imsOverview, imsOverviewColor),
            ("通信", communicationSummary, communicationColor),
        ]
        return items
    }

    private func statusColumn(_ title: String, _ value: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 4 : 7) {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            HStack(spacing: 6) {
                Circle().fill(color).frame(width: 5, height: 5)
                Text(value)
                    .font(.system(size: 12, weight: .semibold))
                    .lineLimit(1)
                    .minimumScaleFactor(0.75)
                    .layoutPriority(1)
                    .copyableInformation(value)
                    .help("点击复制：\(value)")
            }
        }
        .padding(.horizontal, 2)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(title)：\(value)，点击状态值可复制")
    }

    private var deviceActivityCard: some View {
        activityPanel(title: "设备", icon: "externaldrive.connected.to.line.below", tint: ProletariusPalette.module) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(connectedDeviceCount)")
                    .font(.system(size: viewport.isCompactHeight ? 30 : 40, weight: .bold, design: .rounded))
                    .foregroundStyle(WorkspaceColors.accent)
                    .monospacedDigit()
                Text("当前连接").font(.caption).foregroundStyle(.secondary)
            }
            Divider()
            activityRow("DJI 模块", moduleStatusText == "已连接" ? moduleOperator : moduleStatusText,
                        moduleStatusText == "已连接" ? .green : .secondary)
            activityRow("读卡器", store.readerConnected ? readerOperator : "未连接", store.readerConnected ? .green : .secondary)
        }
    }

    private var voWiFiActivityCard: some View {
        activityPanel(title: "Wi-Fi Calling", icon: "wifi", tint: ProletariusPalette.reader) {
            Text(imsSummary).font(.system(size: viewport.isCompactHeight ? 15 : 20, weight: .semibold))
            Text(tunnelSummary).font(.caption).foregroundStyle(.secondary)
            Divider()
            activityRow("SIM", store.readerVoWiFiStatus?.profileName ?? readerOperator, store.readerConnected ? .green : .secondary)
            activityRow("ePDG", epdgSummary, tunnelColor)
            activityRow("媒体", store.readerAudioPhase == "未连接" ? "无活动通话" : store.readerAudioPhase, store.audioRunning ? .green : .secondary)
        }
    }

    private var communicationActivityCard: some View {
        activityPanel(title: "通信", icon: "message.badge.waveform", tint: ProletariusPalette.messages) {
            HStack(alignment: .firstTextBaseline, spacing: 7) {
                Text("\(smsStore.items.count)")
                    .font(.system(size: viewport.isCompactHeight ? 30 : 40, weight: .bold, design: .rounded))
                    .foregroundStyle(WorkspaceColors.accent)
                    .monospacedDigit()
                Text("条短信").font(.caption).foregroundStyle(.secondary)
            }
            Divider()
            activityRow("未读短信", "\(attentionStore.unreadSMSCount)", attentionStore.unreadSMSCount > 0 ? .orange : .secondary)
            activityRow("未查看电话", "\(attentionStore.unviewedCallCount)", attentionStore.unviewedCallCount > 0 ? .orange : .secondary)
            activityRow("通话状态", callSummary, store.callStatus.isActive ? .green : .secondary)
        }
    }

    private var trafficActivityCard: some View {
        activityPanel(title: "DJI 模块网络", icon: "arrow.up.arrow.down", tint: ProletariusPalette.diagnostics) {
            HStack(spacing: viewport.isCompactHeight ? 18 : 28) {
                trafficMetric("下载", store.traffic?.sessionRX, color: .cyan)
                trafficMetric("上传", store.traffic?.sessionTX, color: .indigo)
            }
            Divider()
            activityRow("会话总计", byteText(store.traffic?.sessionTotal), store.traffic?.available == true ? .green : .secondary)
            activityRow("网络接口", store.traffic?.interface ?? "未连接", store.traffic?.available == true ? .green : .secondary)
        }
    }

    private func activityPanel<Content: View>(
        title: String,
        icon: String,
        tint: Color,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 7 : 14) {
            HStack(spacing: 10) {
                WorkspaceSymbol(systemName: icon, tint: WorkspaceColors.accent, size: viewport.isCompactHeight ? 28 : 36)
                Text(title).font(.system(size: 15, weight: .semibold))
            }
            content()
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, minHeight: viewport.isCompactHeight ? 170 : 210, alignment: .topLeading)
        .workspaceCard(tint: tint)
    }

    private func activityRow(_ title: String, _ value: String, _ color: Color) -> some View {
        HStack(spacing: 8) {
            Text(title).foregroundStyle(.secondary)
            Spacer()
            Circle().fill(color).frame(width: 6, height: 6)
            Text(value)
                .lineLimit(1)
                .truncationMode(.middle)
                .copyableInformation(value)
        }
        .font(.system(size: 12))
        .padding(.vertical, 2)
    }

    private func trafficMetric(_ title: String, _ bytes: UInt64?, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(byteText(bytes)).font(.title3.weight(.semibold).monospacedDigit()).foregroundStyle(color)
        }
    }

    private var moduleConnected: Bool {
        guard backend.state == .running, let status = store.status else { return false }
        guard status.hardwareStatus?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false else { return false }
        return MenuBarStatusPolicy.moduleHasControlResponse(
            imei: status.imei, iccid: status.iccid, imsi: status.imsi,
            operatorName: status.operatorName, networkMode: status.networkMode,
            simInserted: status.simInserted, signalDbm: status.signalDbm,
            registrationStatus: status.regStatus)
    }

    private var moduleStatusText: String {
        MenuBarStatusPolicy.moduleStatus(serviceRunning: backend.state == .running,
            hasStatus: store.status != nil, stale: store.statusStale,
            hardwareIssue: store.status?.hardwareStatus?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false,
            hardwarePresent: moduleConnected,
            enumeratedDevicePresent: store.status?.usbDevice != nil,
            simInserted: store.status?.simInserted,
            qmiSessionInitialized: store.status?.qmi?.sessionInitialized)
    }

    private var connectedDeviceCount: Int {
        (moduleConnected && !store.statusStale ? 1 : 0) + (backend.state == .running && store.readerConnected ? 1 : 0)
    }

    private var moduleOperator: String {
        let value = store.status?.operatorName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return value.isEmpty ? "已连接" : value
    }

    private var readerOperator: String {
        if let name = store.readerProfileOperatorName?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
            return name
        }
        return readerService.displayName(forReader: store.readerName ?? "PC/SC 读卡器")
    }

    private var imsSummary: String {
        imsOverview
    }

    private var deviceSummaryColor: Color {
        connectedDeviceCount == 0 ? .secondary : .green
    }

    private var imsOverview: String {
        guard backend.state == .running else { return "状态未知" }
        if moduleConnected && store.readerConnected {
            return "模块 \(moduleIMSShort) · 读卡器 \(readerIMSShort)"
        }
        if moduleConnected { return "模块 \(moduleIMSShort)" }
        if store.readerConnected { return "读卡器 \(readerIMSShort)" }
        return "未连接"
    }

    private var moduleIMSShort: String {
        guard backend.state == .running else { return "状态未知" }
        guard !store.statusStale else { return "数据过期" }
        guard moduleConnected else { return "未连接" }
        let status = store.voWiFiStatus
        return MenuBarStatusPolicy.imsStatus(loaded: store.voWiFiLoaded,
            failed: status?.state == "failed" || status?.state == "unavailable",
            registered: status?.vowifiActive == true && status?.imsRegistered == true,
            smsReady: status?.smsReady, callReady: status?.callReady,
            connecting: status?.vowifiActive == true || store.voWiFiActionInFlight)
    }

    private var readerIMSShort: String {
        guard backend.state == .running else { return "状态未知" }
        guard store.readerConnected else { return "未连接" }
        let status = store.readerVoWiFiStatus
        return MenuBarStatusPolicy.imsStatus(loaded: store.readerVoWiFiLoaded,
            failed: status?.imsState == "failed" || status?.imsState == "error",
            registered: status?.imsState == "registered", smsReady: status?.smsReady, callReady: status?.callReady,
            connecting: status?.imsState == "registering" || status?.tunnelState == "connecting")
    }

    private var imsOverviewColor: Color {
        let states = [moduleIMSShort, readerIMSShort]
        if states.contains("异常") {
            return .red
        }
        if states.contains("已就绪") {
            return .green
        }
        if states.contains("连接中") || states.contains("恢复中") {
            return .blue
        }
        return .secondary
    }

    private var communicationSummary: String {
        "\(attentionStore.unreadSMSCount) 未读 · \(callSummary)"
    }

    private var communicationColor: Color {
        if attentionStore.unreadSMSCount > 0 { return .orange }
        if store.callStatus.isActive { return .green }
        if store.callStatus.state == "incoming" { return .orange }
        if ["dialing", "alerting"].contains(store.callStatus.state) { return .blue }
        return .secondary
    }

    private var tunnelSummary: String {
        guard store.readerConnected else { return "连接读卡器后可用" }
        switch store.readerVoWiFiStatus?.tunnelState {
        case "connected": return "IKEv2 / ESP 隧道已建立"
        case "connecting": return "正在建立安全隧道"
        case "error": return "安全隧道建立失败"
        default: return "Wi-Fi Calling 未启动"
        }
    }

    private var tunnelColor: Color {
        switch store.readerVoWiFiStatus?.tunnelState {
        case "connected": return .green
        case "connecting": return .blue
        case "error": return .red
        default: return .secondary
        }
    }

    private var epdgSummary: String {
        let value = store.readerVoWiFiStatus?.epdgFqdn?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return value.isEmpty ? "未建立" : value
    }

    private var callSummary: String {
        MenuBarStatusPolicy.callStatus(serviceRunning: backend.state == .running, state: store.callStatus.state)
    }


    private var backendSummary: String {
        switch backend.state {
        case .running: return "正常"
        case .starting: return "启动中"
        case .stopping: return "正在停止"
        case .stopped: return "已停止"
        case .failed: return "异常"
        }
    }

    private var backendColor: Color {
        switch backend.state {
        case .running: return .green
        case .starting: return .blue
        case .stopping: return .orange
        case .stopped: return .secondary
        case .failed: return .red
        }
    }

    private func byteText(_ bytes: UInt64?) -> String {
        guard store.traffic?.available == true, let bytes else { return "-" }
        return ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
}

/// 服务未运行时占位页
struct ServiceOffView: View {
    @EnvironmentObject private var backend: BackendProcess

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "power")
                .font(.system(size: 44))
                .foregroundStyle(.tertiary)
            Text(backend.state == .stopping ? "正在安全退出" : "服务未运行")
                .font(.title3)
            if backend.state == .stopping {
                Text("正在等待后台完成资源清理。")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else if case .failed(let reason) = backend.state, !reason.isEmpty {
                Text(reason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 400)
            } else {
                Text("服务启动异常，请稍后重试。")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Button("重试") {
                backend.start()
            }
            .buttonStyle(.borderedProminent)
            .disabled(backend.state == .stopping)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
