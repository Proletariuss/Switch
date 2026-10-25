import SwiftUI

enum DevicePageScope {
    case module
    case reader
}

/// 模块与读卡器共用同一状态源，但在导航中各自呈现，避免重复枚举或复制 VoWiFi 会话状态。
struct HomeView: View {
    @EnvironmentObject private var backend: BackendProcess
    @EnvironmentObject private var store: DashboardStore
    @EnvironmentObject private var smsStore: SMSStore
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.workspaceViewport) private var viewport

    @State private var showingRebootConfirm = false
    @State private var showingModuleESIM = false
    @StateObject private var moduleESIM = ModuleESIMStore()
    @State private var copiedValue: String?
    @State private var draggedService: Int?
    @State private var showNotifyDeniedAlert = false
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @AppStorage("readerEnabled") private var readerEnabled = false

    private var status: DeviceStatus? { store.status }
    private var health: HealthStatus? { store.health }
    private var traffic: TrafficSnapshot? { store.traffic }
    private var moduleConnected: Bool {
        guard !store.statusStale, let status else { return false }
        if let hardwareStatus = status.hardwareStatus,
           !hardwareStatus.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return false
        }
        return MenuBarStatusPolicy.moduleHasControlResponse(
            imei: status.imei, iccid: status.iccid, imsi: status.imsi,
            operatorName: status.operatorName, networkMode: status.networkMode,
            simInserted: status.simInserted, signalDbm: status.signalDbm,
            registrationStatus: status.regStatus)
    }
    let scope: DevicePageScope

    init(scope: DevicePageScope = .module) {
        self.scope = scope
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                if scope == .reader {
                    readerPageContent
                } else {
                    modulePageContent
                }
            }
            .padding(viewport.pageInset)
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.proletarius)
        .scrollIndicators(.visible)
        .scrollBounceBehavior(.basedOnSize)
        .overlay(alignment: .bottomTrailing) {
            if let toast = store.toast {
                ToastBubble(toast: toast) {
                    store.toast = nil
                }
                .onHover { hovering in
                    if hovering {
                        store.extendToast()
                    }
                }
                .padding(16)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(reduceMotion ? nil : ProletariusMotion.stateChange, value: store.callStatus)
        .animation(reduceMotion ? nil : ProletariusMotion.stateChange, value: store.toast)
        .onAppear {
            if scope == .module {
                store.networkServices.load()
                store.moduleNetwork.refreshFlightMode()
            }
            // 读卡器页进入时立即同步；后台热插拔轮询仍独立运行。
            if scope == .reader, case .running = backend.state {
                readerEnabled = store.readerConnected || !store.readerList.isEmpty
                store.refreshReaderList()
            }
        }
        .compatibleOnChange(of: backend.state) { newState in
            if scope == .reader, case .running = newState {
                store.refreshReaderList()
            }
        }
        .compatibleOnChange(of: store.readerList) { readers in
            guard scope == .reader else { return }
            readerEnabled = !readers.isEmpty
        }
        .compatibleOnChange(of: readerEnabled) { enabled in
            if scope == .reader, enabled, case .running = backend.state {
                store.refreshReaderList()
            }
        }
        .confirmationDialog("重启模块？", isPresented: $showingRebootConfirm, titleVisibility: .visible) {
            Button("重启（AT+CFUN=1,1）", role: .destructive) { store.reboot() }
            Button("取消", role: .cancel) {}
        } message: {
            Text("模块重启会导致网络临时中断，正在进行的下载或写入操作会被打断。")
        }
        .sheet(isPresented: $showingModuleESIM) {
            ModuleESIMView(
                store: moduleESIM,
                activeICCID: status?.iccid,
                activePhoneNumber: status?.phoneNumber) {
                if moduleESIM.operation?.kind == "switch_profile" {
                    store.moduleVoWiFi.invalidateAfterProfileSwitch()
                }
                store.refresh()
                if scope == .module { store.moduleNetwork.refreshFlightMode() }
            }
        }
        .compatibleOnChange(of: moduleConnected) { connected in
            if !connected { moduleESIM.clear() }
            if scope == .module { store.moduleNetwork.refreshFlightMode() }
        }
        .compatibleOnChange(of: status?.regStatus) { _ in
            if scope == .module { store.moduleNetwork.refreshFlightMode() }
        }
    }

    @ViewBuilder
    private var modulePageContent: some View {
        WorkspaceHero(
            title: "管理连接与号卡",
            subtitle: "蜂窝网络、Wi-Fi Calling 与模块配置，在同一工作区清晰呈现。",
            symbol: "antenna.radiowaves.left.and.right",
            eyebrow: "DEVICE / MODULE")
        ModuleRecoveryStatusView()
        switch backend.state {
        case .running:
            if status != nil {
                overviewBar
                moduleWorkspaceColumns
            } else if store.statusStale {
                VStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 34)).foregroundStyle(.orange)
                    Text("模块状态获取失败").font(.headline)
                    Text("请确认模块已连接，然后重试。").font(.callout).foregroundStyle(.secondary)
                    Button("重试") {
                        store.refresh()
                    }
                    .buttonStyle(.proletariusPrimary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 40)
            } else {
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text("读取模块状态…").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 60)
            }
        case .starting:
            VStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text("正在启动服务…").foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 60)
        case .stopping:
            ProgressView("正在安全退出，等待后台完成资源清理…")
                .frame(maxWidth: .infinity)
                .padding(.vertical, 60)
        case .stopped:
            VStack(spacing: 10) {
                Image(systemName: "power").font(.system(size: 34)).foregroundStyle(.tertiary)
                Text("服务未运行").font(.headline)
                Text("请退出并重新打开应用").font(.callout).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 60)
        case .failed(let reason):
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 34)).foregroundStyle(.orange)
                Text("服务启动失败").font(.headline)
                Text(reason)
                    .font(.callout).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button("重试") {
                    backend.start()
                }
                .buttonStyle(.proletariusPrimary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 40)
        }
    }

    /// Independent columns avoid leaving blank space under shorter cards.
    /// No field or control is hidden when height is limited: the containing
    /// scroll view remains the overflow fallback for long errors/service lists.
    @ViewBuilder
    private var moduleWorkspaceColumns: some View {
        let columns = viewport.columnCount(minimumWidth: 318)
        if columns >= 3 {
            HStack(alignment: .top, spacing: viewport.sectionSpacing) {
                VStack(spacing: viewport.sectionSpacing) {
                    deviceCard
                    smsCard
                }
                .frame(maxWidth: .infinity)
                voWiFiCard.frame(maxWidth: .infinity, alignment: .topLeading)
                VStack(spacing: viewport.sectionSpacing) {
                    trafficCard
                    priorityCard
                }
                .frame(maxWidth: .infinity)
            }
        } else if columns == 2 {
            HStack(alignment: .top, spacing: viewport.sectionSpacing) {
                VStack(spacing: viewport.sectionSpacing) {
                    deviceCard
                    smsCard
                }
                .frame(maxWidth: .infinity)
                VStack(spacing: viewport.sectionSpacing) {
                    voWiFiCard
                    trafficCard
                    priorityCard
                }
                .frame(maxWidth: .infinity)
            }
        } else {
            VStack(spacing: viewport.sectionSpacing) {
                deviceCard
                voWiFiCard
                trafficCard
                smsCard
                priorityCard
            }
        }
    }

    @ViewBuilder
    private var readerPageContent: some View {
        WorkspaceHero(
            title: "读卡器",
            subtitle: "读卡器连接、实体号卡与 VoWiFi 状态。",
            symbol: "creditcard.and.123",
            eyebrow: "DEVICE / READER")
        switch backend.state {
        case .running:
            ReaderCardView(
                store: store,
                readerEnabled: $readerEnabled,
                privacyMaskEnabled: privacyMaskEnabled,
                reduceMotion: reduceMotion) {
                voWiFiCard
            }
        case .starting:
            ProgressView("正在启动读卡器服务…")
                .frame(maxWidth: .infinity)
                .padding(.vertical, 60)
        case .stopping:
            ProgressView("正在安全退出，等待后台完成资源清理…")
                .frame(maxWidth: .infinity)
                .padding(.vertical, 60)
        case .stopped:
            readerServiceUnavailable("服务未运行", detail: "请退出并重新打开应用")
        case .failed(let reason):
            readerServiceUnavailable("读卡器服务启动失败", detail: reason)
        }
    }

    private func readerServiceUnavailable(_ title: String, detail: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: "creditcard.trianglebadge.exclamationmark")
                .font(.system(size: 34))
                .foregroundStyle(.orange)
            Text(title).font(.headline)
            Text(detail).font(.callout).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 60)
    }

    // MARK: - 顶部概览条

    private var overviewBar: some View {
        let vowifi = store.voWiFiStatus
        let moduleUsesWiFiCalling = vowifi?.imsWiFiCallingEnabled == true && vowifi?.airplaneMode == true
        let moduleRadioOff = store.moduleNetwork.flightModeStatus?.enabled == true
        let moduleIsolated = status?.discoveryError == "legacy_recovery_required"
        return VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 8 : 14) {
            HStack(alignment: .center, spacing: 12) {
                WorkspaceSymbol(systemName: "externaldrive", tint: WorkspaceColors.accent, size: 36)
                if let status, !moduleConnected {
                    let qmiInitialized = !store.statusStale && status.qmi?.sessionInitialized == true
                    let detail = status.hardwareStatus?.trimmingCharacters(in: .whitespacesAndNewlines)
                    let message: String = if store.statusStale {
                        "无法确认当前 USB、AT 或 QMI 状态；请刷新后再操作模块"
                    } else if moduleIsolated {
                        "模块因旧版恢复证据而暂停 AT/QMI；这不代表 USB 未插入。请查看上方原因码。"
                    } else if qmiInitialized {
                        "QMI 会话已完成初始化；当前链路、SIM、注册和网络状态尚未实时确认"
                    } else {
                        detail.flatMap { $0.isEmpty ? nil : $0 }
                            ?? (status.usbDevice == nil
                                ? "未检测到模块控制接口"
                                : "USB 设备已枚举，但尚未收到 DJI 模块控制响应")
                    }
                    let title: String = if store.statusStale {
                        "模块数据已过期"
                    } else if moduleIsolated {
                        "模块已隔离，等待恢复核对"
                    } else if qmiInitialized {
                        "DJI QMI 会话已初始化"
                    } else if status.usbDevice == nil {
                        "模块未就绪"
                    } else {
                        "已检测到 USB 模块，控制通道未就绪"
                    }
                    VStack(alignment: .leading, spacing: 3) {
                        Text(title)
                            .font(.headline)
                        Text(moduleDetectionDetails(fallback: message))
                            .font(.caption).foregroundStyle(.secondary).lineLimit(3)
                    }
                    Spacer(minLength: 0)
                } else {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(moduleUsesWiFiCalling
                             ? (vowifi?.operatorName ?? status?.operatorName ?? "Wi-Fi Calling")
                             : (status?.operatorName ?? "-"))
                            .font(.headline)
                        Text(moduleUsesWiFiCalling
                             ? "\(vowifi?.imsRegistered == true ? "IMS 已注册" : "IMS 正在连接") · 蜂窝射频已关闭"
                             : (moduleRadioOff
                                ? "飞行模式 · 蜂窝射频收发已关闭，不扫描基站"
                                : "\(status?.regStatusText ?? "-") · \(nativePacketStatusText) · 更新于 \(store.lastUpdated?.formatted(date: .omitted, time: .standard) ?? "-")"))
                            .font(.caption).foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 8)
                    Button("重启模块") { showingRebootConfirm = true }
                        .controlSize(.small)
                        .disabled(store.busy || store.networkRecovering)
                }
            }
            if store.statusStale {
                ProletariusStatusBadge(title: "数据已过期", color: .orange)
            }
            if moduleConnected {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 12)], alignment: .leading, spacing: 10) {
                    if !moduleUsesWiFiCalling, !moduleRadioOff, let dbm = status?.signalDbm {
                        HStack(spacing: 6) {
                            signalBars(dbm: dbm)
                            Text("\(dbm) dBm").font(.caption.monospacedDigit())
                        }
                    }
                    if !moduleUsesWiFiCalling, !moduleRadioOff, let mode = status?.networkMode, !mode.isEmpty {
                        ProletariusStatusBadge(title: mode, color: WorkspaceColors.accent)
                    }
                    HStack(spacing: 5) {
                        Circle().fill(status?.simInserted == true ? Color.green : Color.orange)
                            .frame(width: 6, height: 6)
                        Text(moduleUsesWiFiCalling ? "Wi-Fi Calling" : (moduleRadioOff ? "蜂窝射频关闭" : MenuBarStatusPolicy.moduleSIMLabel(status?.simInserted)))
                            .font(.caption)
                    }
                    .foregroundStyle(.secondary)
                }
            }
        }
        .padding(viewport.cardPadding)
        .glassCardTinted(.accentColor)
    }

    private func moduleDetectionDetails(fallback: String) -> String {
        guard !store.statusStale else { return fallback }
        guard let status else { return fallback }
        let device = status.usbDevice
        let product = [device?.vendor, device?.product]
            .compactMap { value -> String? in
                guard let value else { return nil }
                let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
                return trimmed.isEmpty ? nil : trimmed
            }
            .joined(separator: " ")
        let identity = [device?.vendorID, device?.productID]
            .compactMap { value -> String? in
                guard let value, !value.isEmpty else { return nil }
                return value
            }
            .joined(separator: ":")
        var details: [String] = []
        let qmiError = status.qmi?.sessionInitialized == true ? nil
            : status.qmi?.initializationError.map { "QMI: \($0)" }
        for value in [product, identity.isEmpty ? nil : "VID:PID \(identity)", device?.mode,
                      status.discoveryError, fallback, qmiError].compactMap({ $0 }) {
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty && !details.contains(trimmed) {
                details.append(trimmed)
            }
        }
        return details.joined(separator: " · ")
    }

    // MARK: - 设备信息

    private var deviceCard: some View {
        let liveStatus = moduleConnected ? status : nil
        return VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 10 : 18) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "simcard", tint: WorkspaceColors.accent,
                                size: viewport.isCompactHeight ? 32 : 40)
                WorkspaceSectionHeader(title: "模块与 SIM")
                Button {
                    showingModuleESIM = true
                } label: {
                    Label("eSIM 管理", systemImage: "simcard.2")
                }
                .buttonStyle(.proletarius)
                .controlSize(.small)
                .disabled(health?.esimAvailable != true || !moduleConnected)
            }
            row("当前号码", liveStatus?.phoneNumber ?? "-", sensitive: true)
            Divider().opacity(0.55)
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 126), spacing: 12)], alignment: .leading, spacing: viewport.isCompactHeight ? 10 : 16) {
				row("ICCID", liveStatus?.iccid ?? "-", sensitive: true)
				row("IMSI", liveStatus?.imsi ?? "-", sensitive: true)
                row("IMEI", liveStatus?.imei ?? "-", sensitive: true)
                row("固件", liveStatus?.firmware ?? "-")
                row("AT 端口", store.statusStale ? "-" : (health?.port ?? "-"), sensitive: true)
            }
            Divider().opacity(0.55)
            HStack(spacing: 8) {
                Label(MenuBarStatusPolicy.moduleSIMLabel(liveStatus?.simInserted),
                      systemImage: liveStatus?.simInserted == true ? "simcard.fill" : "simcard")
                    .font(.caption)
                    .foregroundStyle(liveStatus?.simInserted == true ? Color.green : Color.secondary)
                Spacer()
            }
            Divider().opacity(0.55)
            moduleFlightModeRow
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .glassCard()
    }

    private var moduleFlightModeRow: some View {
        let flight = store.moduleNetwork.flightModeStatus
        let enabled = flight?.enabled == true
        let vowifiOwnsRadio = flight?.owner == "vowifi"
        let detail: String = {
            if let error = store.moduleNetwork.flightModeError, !error.isEmpty {
                return error
            }
            if vowifiOwnsRadio {
                return "蜂窝射频由 Wi-Fi Calling 管理"
            }
            if enabled, flight?.signalKnown == true,
               flight?.signalDetected == false, flight?.cellularRegistered == false,
               flight?.verified == true {
                return flight?.userConfigured == true
                    ? "已按你的选择关闭蜂窝射频；无信号且未注册基站"
                    : "默认保护已生效：无蜂窝信号且未注册基站"
            }
            return flight?.detail ?? "启用后关闭模块蜂窝射频"
        }()

        return VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                Label("飞行模式", systemImage: "airplane")
                    .font(.callout.weight(.medium))
                Spacer(minLength: 8)
                if store.moduleNetwork.flightModeBusy {
                    ProgressView().controlSize(.small)
                }
                Toggle("飞行模式", isOn: Binding(
                    get: { store.moduleNetwork.flightModeStatus?.enabled == true },
                    set: { store.moduleNetwork.setFlightMode(enabled: $0) }))
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .tint(.blue)
                    .controlSize(.small)
                    .disabled(!moduleConnected || !store.moduleNetwork.flightModeLoaded ||
                              store.moduleNetwork.flightModeBusy || flight?.available != true ||
                              flight?.canToggle != true || store.voWiFiActionInFlight)
                    .accessibilityLabel("模块飞行模式")
            }
            Text(detail)
                .font(.caption)
                .foregroundStyle(enabled && flight?.verified == true
                                 ? Color.green : Color.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
    }

    private var nativePacketStatusText: String {
        switch status?.psAttached {
        case true: return "数据网络已附着"
        case false: return "数据网络未附着"
        case nil: return "数据状态未知"
        }
    }

    // MARK: - 实时流量

    private var trafficCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 10) {
                    sectionTitle("网络与流量")
                    Spacer(minLength: 8)
                    trafficActions
                }
                VStack(alignment: .leading, spacing: 8) {
                    sectionTitle("网络与流量")
                    HStack(spacing: 8) { trafficActions }
                }
            }
            if let message = store.moduleNetwork.actionError ?? store.moduleNetwork.actionMessage {
                Label(message, systemImage: store.moduleNetwork.actionError != nil ? "exclamationmark.triangle" : "info.circle")
                    .font(.caption)
                    .foregroundStyle(store.moduleNetwork.actionError != nil ? Color.orange : Color.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            if let traffic {
                if !traffic.available {
                    networkRecoveryRow(traffic)
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: 10)], spacing: 10) {
                    trafficItem("本次下载", trafficMetric(traffic.sessionRX))
                    trafficItem("本次上传", trafficMetric(traffic.sessionTX))
                    trafficItem("本次总流量", trafficMetric(traffic.sessionTotal))
                    trafficItem("网卡累计", trafficMetric(trafficTotal))
                }
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("读取流量数据…").foregroundStyle(.secondary)
                }
                .padding(.vertical, 12)
            }
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .glassCard()
    }

    @ViewBuilder
    private var trafficActions: some View {
        if store.busy && !store.networkRecovering {
            ProgressView().controlSize(.small)
        }
        Button("检查 4G 出口") { store.runCheck4G() }
            .controlSize(.small)
            .disabled(store.busy || store.networkRecovering)
            .fixedSize()
        Button(store.usbnetEnabled ? "关闭网卡" : "开启网卡") {
            store.switchMode()
        }
        .controlSize(.small)
        .disabled(store.busy || store.networkRecovering || !store.usbnetLoaded)
        .fixedSize()
    }

    // MARK: - 网络恢复

    /// 网卡不可用时显示恢复进度行
    @ViewBuilder
    private func networkRecoveryRow(_ traffic: TrafficSnapshot) -> some View {
        if store.networkRecovering {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(store.networkRecoveryMessage ?? "正在恢复网络…")
                    .font(.callout)
                    .foregroundStyle(.orange)
                Spacer()
            }
        } else if store.canRetryNetworkConnection {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .font(.callout)
                Text("网卡连接异常")
                    .font(.callout)
                    .foregroundStyle(.orange)
                Spacer()
                Button("重试") {
                    store.retryNetworkConnection()
                }
                .controlSize(.small)
            }
        } else {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .font(.callout)
                Text("网卡不可用")
                    .font(.callout)
                    .foregroundStyle(.orange)
                Spacer()
            }
        }
    }

    // MARK: - 网卡优先级

    private var priorityCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                sectionTitle("网卡优先级")
                Spacer()
                Button("应用顺序") {
                    store.networkServices.saveOrder()
                }
                .controlSize(.small)
                .disabled(store.networkServices.services.count < 2 || store.networkServices.isSaving)
                Button {
                    store.networkServices.load()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .controlSize(.small)
                .help("刷新服务列表")
            }
            if store.networkServices.isLoaded {
                if store.networkServices.services.isEmpty {
                    Text("未读取到网络服务").font(.caption).foregroundStyle(.secondary)
                } else {
                    VStack(spacing: 4) {
                        ForEach(Array(store.networkServices.services.enumerated()), id: \.element.id) { index, service in
                            ServiceRowView(
                                index: index,
                                service: service,
                                isDragging: draggedService == index)
                            .onDrag {
                                draggedService = index
                                return NSItemProvider(object: service.name as NSString)
                            } preview: {
                                Text(service.name)
                                    .font(.callout)
                                    .padding(.horizontal, 12).padding(.vertical, 6)
                                    .glassPill(cornerRadius: 6)
                                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.accentColor, lineWidth: 1))
                            }
                            .onDrop(
                                of: [.text],
                                delegate: ServiceDropDelegate(
                                    destination: index,
                                    dragged: $draggedService,
                                    onMove: { from, to in
                                        store.networkServices.moveService(from: from, to: to)
                                    }))
                        }
                    }
                    HStack(spacing: 10) {
                        if let orderMessage = store.networkServices.message {
                            Text(orderMessage).font(.caption).foregroundStyle(.green)
                        }
                        if let orderError = store.networkServices.error {
                            Text(orderError).font(.caption).foregroundStyle(.red)
                                .lineLimit(2)
                        }
                        Spacer()
                    }
                }
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("读取网络服务…").foregroundStyle(.secondary)
                }
            }
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .glassCard()
    }

    // MARK: - 短信保存

    private var smsCard: some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 8 : 14) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "tray", tint: WorkspaceColors.accent,
                                size: viewport.isCompactHeight ? 32 : 40)
                WorkspaceSectionHeader(title: "短信保存")
            }
            switchRow("接管模块短信", isOn: Binding(
                get: { smsStore.adoptionEnabled },
                set: { smsStore.setAdoptionEnabled($0) }
            ), isEnabled: !smsStore.adoptionUpdating)
            switchRow("新短信通知", isOn: $smsStore.notificationsEnabled) {
                requestNotificationAuth()
            }
            if let storage = smsStore.storage {
                HStack(spacing: 8) {
                    Text("存储").font(.caption).foregroundStyle(.secondary)
                    storageBadge("SIM 卡", storage.usage?["SM"])
                    storageBadge("模块", storage.usage?["ME"])
                }
            }
            Text(smsStore.adoptionEnabled
                 ? "已接管：收到的短信保存到本机，SIM 卡与模块存储可按需手动清理。"
                 : "未接管：短信保留在 SIM 卡与模块存储中，由模块存储管理。")
                .font(.caption)
                .foregroundStyle(.secondary)
            if let error = smsStore.adoptionError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassCard()
        .alert("通知权限被拒绝", isPresented: $showNotifyDeniedAlert) {
            Button("好", role: .cancel) {}
        } message: {
            Text("请在 系统设置 → 通知 中允许 Switch 的通知后再开启。")
        }
    }

    /// 标题在左、开关在右的一行
    private func switchRow(
        _ title: String,
        isOn: Binding<Bool>,
        isEnabled: Bool = true,
        onChange: (() -> Void)? = nil
    ) -> some View {
        HStack(spacing: 8) {
            Text(title).font(.callout)
            Spacer()
            Toggle("", isOn: isOn)
                .toggleStyle(.switch)
                .controlSize(.small)
                .labelsHidden()
                .accessibilityLabel(title)
                .compatibleOnChange(of: isOn.wrappedValue) { on in
                    if on { onChange?() }
                }
                .disabled(!isEnabled)
        }
    }

    private func storageBadge(_ title: String, _ usage: SMSStorageUsage?) -> some View {
        guard let usage else {
            return AnyView(EmptyView())
        }
        let full = usage.used >= usage.total && usage.total > 0
        return AnyView(
            Text("\(title) \(usage.used)/\(usage.total)")
                .font(.caption)
                .foregroundStyle(full ? Color.red : Color.secondary)
                .padding(.horizontal, 6).padding(.vertical, 2)
                .background(Capsule().fill(full ? Color.red.opacity(0.12) : Color.gray.opacity(0.12)))
        )
    }

    private func requestNotificationAuth() {
        Task {
            if await !smsStore.ensureAuthorization() {
                smsStore.notificationsEnabled = false
                showNotifyDeniedAlert = true
            }
        }
    }

    // MARK: - Wi-Fi Calling

    private var voWiFiCard: some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 10 : 18) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "wifi", tint: WorkspaceColors.accent,
                                size: viewport.isCompactHeight ? 32 : 40)
                WorkspaceSectionHeader(
                    title: "Wi-Fi Calling",
                    detail: scope == .reader ? "读卡器 · 当前号卡的通信连接" : "模块 · 通过 Mac 网络承载通信")
            }

            if scope == .reader {
                if store.readerConnected {
                    // Preserve the configuration surface while a card refresh
                    // temporarily has no identity. Never enable from old data.
                    voWiFiReaderMode(usim: store.readerUSIMInfo)
                } else {
                    Text("请先连接 PC/SC 读卡器")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                }
            } else if moduleConnected {
                voWiFiModuleMode
            } else {
                Text("DJI 模块未连接，Wi-Fi Calling 操作不可用")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 12)
            }
        }
        .padding(viewport.cardPadding)
        .glassCard()
        .onAppear {
            if scope == .reader, store.readerConnected {
                store.refreshReaderVoWiFi()
            } else if scope == .module, moduleConnected {
                store.refreshVoWiFi()
            }
        }
        .compatibleOnChange(of: store.readerConnected) { connected in
            guard scope == .reader else { return }
            if connected {
                store.refreshReaderVoWiFi()
            }
        }
    }

    // MARK: Wi-Fi Calling - 模块模式

    private var voWiFiModuleMode: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !store.voWiFiLoaded {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("读取 Wi-Fi Calling 状态…").foregroundStyle(.secondary)
                }
            } else if let st = store.voWiFiStatus {
                HStack(spacing: 6) {
                    Circle().fill(moduleVoWiFiStatusColor(st)).frame(width: 8, height: 8)
                    Text(moduleVoWiFiStatusTitle(st)).font(.callout.weight(.semibold))
                    Spacer()
                    if st.vowifiActive {
                        Label("已开启", systemImage: "checkmark.circle.fill")
                            .font(.caption)
                            .foregroundStyle(.green)
                    }
                }

                if let opName = st.operatorName, !opName.isEmpty {
                    let flag = (st.operatorMCC.flatMap { mccFlagEmoji($0) }) ?? ""
                    Text("\(flag)\(flag.isEmpty ? "" : " ")\(opName)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                if let detail = st.detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(st.state == "failed" ? Color.red : Color.secondary)
                        .lineLimit(3)
                }

                if store.voWiFiActionInFlight || st.imsWiFiCallingEnabled || st.state == "failed" {
                    voWiFiProgressSteps(
                        step: (st.progressStep ?? 0) == 0 && store.voWiFiError != nil ? 1 : (st.progressStep ?? 0),
                        isError: st.state == "failed" || store.voWiFiError != nil,
                        inFlight: store.voWiFiActionInFlight,
                        imsRegistered: st.imsRegistered,
                        smsReady: st.smsReady == true,
                        description: st.progressDesc,
                        showDescription: true)
                }

                if st.imsWiFiCallingEnabled || st.vowifiActive {
                    HStack(spacing: 12) {
                        readinessItem("IMS", ready: st.imsRegistered)
                        readinessItem("SMS", ready: st.smsReady == true)
                        readinessItem("通话", ready: st.callReady == true)
                    }
                    .font(.caption)
                }

            } else {
                Text(store.voWiFiError ?? "暂无 Wi-Fi Calling 信息")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
            }

            HStack(spacing: 12) {
                Toggle("Wi-Fi Calling", isOn: Binding(
                    get: { store.moduleVoWiFi.toggleEnabled },
                    set: { enabled in
                        if enabled { store.enableVoWiFi() }
                        else { store.disableVoWiFi() }
                    }))
                    .toggleStyle(.switch)
                    .disabled(store.voWiFiActionInFlight && !store.moduleVoWiFi.canCancelStart)
                if store.voWiFiActionInFlight {
                    ProgressView().controlSize(.small)
                    Text(store.moduleVoWiFi.canCancelStart
                         ? "正在连接 · 点击开关可取消"
                         : (store.voWiFiStatus?.progressDesc ?? "正在设置…"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer()
                Button {
                    store.refreshVoWiFi()
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.caption)
                }
                .buttonStyle(.proletarius)
                .controlSize(.small)
                .help("刷新 Wi-Fi Calling 状态")
            }

            if store.voWiFiStatus != nil {
                Text("启用后模块进入飞行模式，由 Mac 当前网络承载 VoWiFi；关闭后恢复原蜂窝状态。")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let err = store.voWiFiError {
                Text(err)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
        }
    }

    private func moduleVoWiFiStatusTitle(_ status: VoWiFiStatus) -> String {
        if status.vowifiActive { return "Wi-Fi Calling 已开启" }
        if store.voWiFiError != nil { return "启动失败" }
        if status.state == "failed" { return "启动失败" }
        if store.voWiFiActionInFlight { return "正在建立连接" }
        if status.imsRegistered { return "IMS 已注册" }
        return "Wi-Fi Calling 未开启"
    }

    private func moduleVoWiFiStatusColor(_ status: VoWiFiStatus) -> Color {
        if status.vowifiActive { return .green }
        if status.state == "failed" || store.voWiFiError != nil { return .red }
        if store.voWiFiActionInFlight || status.state == "registering" { return .orange }
        return .secondary
    }

    private func readinessItem(_ title: String, ready: Bool) -> some View {
        Label(title, systemImage: ready ? "checkmark.circle.fill" : "minus.circle")
            .foregroundStyle(ready ? Color.green : Color.secondary)
    }

    // MARK: Wi-Fi Calling - 读卡器模式

    private func voWiFiReaderMode(usim: USIMInfo?) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            // 状态灯 + 运营商 + 刷新按钮 + 开关
            HStack {
                Circle()
                    .fill(readerTunnelIndicatorColor)
                    .frame(width: 8, height: 8)
                let flag = usim.flatMap { iccidFlagEmoji($0.iccid) ?? mccFlagEmoji($0.mcc) } ?? ""
                Text(usim == nil ? "正在确认号卡" : "\(flag)\(store.readerOperatorDisplayName)")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(readerTunnelIndicatorColor == .green ? .green :
                                     readerTunnelIndicatorColor == .orange ? .orange :
                                     readerTunnelIndicatorColor == .red ? .red : .primary)
					.copyableInformation(store.readerOperatorDisplayName, enabled: usim != nil)
                Button {
                    store.refreshReaderVoWiFi(force: true)
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.caption)
                }
                .buttonStyle(.proletarius)
                .controlSize(.small)
                .help("刷新 Wi-Fi Calling 状态")
                Spacer()
                Toggle("", isOn: Binding(
                    get: {
                        store.readerVoWiFiStatus?.tunnelState == "connected" ||
                        store.readerVoWiFiActionInFlight
                    },
                    set: { newValue in
                        if newValue {
                            guard usim != nil else { return }
                            store.enableReaderVoWiFi()
                        } else {
                            store.disableReaderVoWiFi()
                        }
                    }
                ))
                .toggleStyle(.switch)
                .controlSize(.small)
                .labelsHidden()
                .accessibilityLabel("读卡器 Wi-Fi Calling")
                .accessibilityIdentifier("reader.vowifi.toggle")
                .disabled(usim == nil && !store.readerVoWiFiActionInFlight && store.readerVoWiFiStatus?.tunnelState != "connected")
                .help(usim == nil ? "确认当前号卡后可开启；进行中的连接仍可中止" : "开启或关闭 Wi-Fi Calling")
            }

            if let profileName = store.readerVoWiFiStatus?.profileName, !profileName.isEmpty {
                HStack(spacing: 4) {
                    Image(systemName: "checkmark.seal")
                        .foregroundStyle(.green)
                    Text("模板：\(profileName)")
                    if store.readerVoWiFiStatus?.profileSource == "3gpp-default" {
                        Text("（3GPP 默认）").foregroundStyle(.secondary)
                    }
                }
                .font(.caption2)
            }

            // 前置代理配置
            VStack(alignment: .leading, spacing: 8) {
                    Text("代理（可选）")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    TextField("IP 地址", text: $store.readerVoWiFiProxyIP)
                        .textFieldStyle(.workspace)
                        .controlSize(.small)
                        .disableAutocorrection(true)
                        .disabled(store.readerVoWiFiActionInFlight)
                        .frame(maxWidth: .infinity)
                    Text(":")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    TextField("端口", text: $store.readerVoWiFiProxyPort)
                        .textFieldStyle(.workspace)
                        .controlSize(.small)
                        .disableAutocorrection(true)
                        .disabled(store.readerVoWiFiActionInFlight)
                        .frame(width: 64)
                    // 保存按钮
                    if store.proxyChecking {
                        ProgressView().controlSize(.small)
                    } else {
                        Button {
                            store.saveProxyAndCheck()
                        } label: {
                            Text("保存")
                                .font(.caption)
                        }
                        .buttonStyle(.proletarius)
                        .controlSize(.small)
                        .help("保存并检测代理")
                    }
                }
                // 代理检测结果行
                if let result = store.proxyCheckResult {
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 4) {
                            if result.ok {
                                Image(systemName: "checkmark.circle.fill")
                                    .font(.caption2)
                                    .foregroundStyle(.green)
                                if let probe = result.probeMessage, !probe.isEmpty {
                                    Text(probe).font(.caption2).foregroundStyle(.secondary).lineLimit(2)
                                }
                                if let flag = result.countryFlag, !flag.isEmpty {
                                    Text(flag).font(.caption)
                                }
                                if let country = result.country, !country.isEmpty {
                                    Text(country).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                                }
                                if let ip = result.exitIP, !ip.isEmpty {
                                    Text("·").font(.caption2).foregroundStyle(.tertiary)
                                    Text(ip).font(.caption2.monospaced()).foregroundStyle(.tertiary).lineLimit(1)
                                }
                                if let latency = result.latencyMs {
                                    Text("·").font(.caption2).foregroundStyle(.tertiary)
                                    Text("\(latency)ms").font(.caption2).foregroundStyle(.tertiary)
                                }
                            } else {
                                Image(systemName: "exclamationmark.triangle")
                                    .font(.caption2)
                                    .foregroundStyle(.orange)
                                if let err = result.error, !err.isEmpty {
                                    Text(err).font(.caption2).foregroundStyle(.orange)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                        }
                        if result.ok, let message = result.udpMessage, !message.isEmpty {
                            HStack(spacing: 4) {
                                Image(systemName: result.udpReady == true ? "checkmark.shield" : "exclamationmark.triangle")
                                    .font(.caption2)
                                    .foregroundStyle(result.udpReady == true ? .green : .orange)
                                Text(message)
                                    .font(.caption2)
                                    .foregroundStyle(result.udpReady == true ? Color.secondary : Color.orange)
                                    .lineLimit(2)
                            }
                        }
                    }
                }
            }

            // 自定义 ePDG 优先；留空时由运营商模板自动选择。
            VStack(alignment: .leading, spacing: 4) {
                if let error = store.readerCarrierConfigError, !error.isEmpty {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                    Text("ePDG")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    TextField(store.readerEPDGFQDN.map { "自动：\($0)" } ?? "自动匹配运营商模板", text: $store.readerVoWiFiEPDG)
                        .textFieldStyle(.workspace)
                        .controlSize(.small)
                        .disableAutocorrection(true)
                        .disabled(store.readerVoWiFiActionInFlight)
                        .frame(maxWidth: .infinity)
                    Text(":")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    TextField("500", text: $store.readerVoWiFiEPDGPort)
                        .textFieldStyle(.workspace)
                        .controlSize(.small)
                        .disableAutocorrection(true)
                        .disabled(store.readerVoWiFiActionInFlight)
                        .frame(width: 64)
                    Button {
                        store.saveEPDGConfig()
                    } label: {
                        Text("保存")
                            .font(.caption)
                    }
                    .buttonStyle(.proletarius)
                    .controlSize(.small)
                    .help("自定义值优先；全部留空时使用运营商模板，IKE 从 UDP 500 开始")
                }
                if store.readerVoWiFiEPDG.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                   let suggested = store.readerVoWiFiStatus?.epdgFqdn ?? store.readerEPDGFQDN,
                   !suggested.isEmpty {
                    Text("自动使用：\(suggested)")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
						.copyableInformation(suggested)
                }
                if let err = store.readerVoWiFiEPDGError {
                    HStack(spacing: 4) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                        Text(err).font(.caption2).foregroundStyle(.orange)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }

            // Keep the five-stage rail in place across card changes. Pending
            // stages describe readiness, never an optimistic connected state.
                voWiFiProgressSteps(
                    step: store.readerVoWiFiStatus?.progressStep ?? 0,
                    isError: store.readerVoWiFiStatus?.tunnelState == "error",
                    inFlight: store.readerVoWiFiActionInFlight,
                    imsRegistered: store.readerVoWiFiStatus?.imsState == "registered",
                    smsReady: store.readerVoWiFiStatus?.smsReady == true,
                    description: store.readerVoWiFiStatus?.progressDesc,
                    showDescription: store.readerVoWiFiActionInFlight)

            // 状态消息
            if let msg = store.readerVoWiFiStatus?.message, !msg.isEmpty {
                Text(msg)
                    .font(.caption)
                    .foregroundStyle(store.readerVoWiFiStatus?.tunnelState == "error" ? .red : .secondary)
                    .fixedSize(horizontal: false, vertical: true)
					.copyableInformation(msg)
            }
        }
    }

    /// 5 阶段进度条: SIM → Access → Tunnel → IMS → SMS
    private func voWiFiProgressSteps(
        step: Int,
        isError: Bool,
        inFlight: Bool,
        imsRegistered: Bool,
        smsReady: Bool,
        description: String?,
        showDescription: Bool
    ) -> some View {
        let stages: [(String, String, Int)] = [
            ("SIM", "simcard", 1),
            ("Access", "lock.shield", 2),
            ("Tunnel", "arrow.left.arrow.right", 3),
            ("IMS", "phone.connection", 4),
            ("SMS", "message", 5),
        ]

        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 0) {
                ForEach(stages, id: \.0) { name, icon, stepNum in
                    let stageStatus = progressStageStatus(
                        stepNum: stepNum, step: step, isError: isError,
                        inFlight: inFlight, imsRegistered: imsRegistered,
                        smsReady: smsReady)

                    progressStepView(icon: icon, label: name, status: stageStatus)

                    if stepNum < 5 {
                        Rectangle()
                            .fill(stageStatus == .done ? Color.green : Color.secondary.opacity(0.3))
                            .frame(height: 2)
                            .frame(maxWidth: .infinity)
                    }
                }
            }

            // 当前阶段描述
            if let desc = description, !desc.isEmpty, showDescription {
                Text(desc)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .transition(.opacity)
            }
        }
        .padding(.top, 2)
    }

    private func progressStageStatus(
        stepNum: Int,
        step: Int,
        isError: Bool,
        inFlight: Bool,
        imsRegistered: Bool,
        smsReady: Bool
    ) -> ProgressStageStatus {
        if isError && stepNum == step { return .failed }
        if stepNum == 4 && !imsRegistered {
            return inFlight && step == 4 ? .current : .pending
        }
        if stepNum == 5 && (!imsRegistered || !smsReady) {
            return inFlight && step == 5 ? .current : .pending
        }
        if stepNum < step { return .done }
        if stepNum == step { return inFlight ? .current : .done }
        return .pending
    }

    /// 单个进度步骤圆点
    private func progressStepView(icon: String, label: String, status: ProgressStageStatus) -> some View {
        VStack(spacing: 7) {
            ZStack {
                RoundedRectangle(cornerRadius: 10)
                    .fill(status.fillColor.opacity(status == .pending ? 0.30 : 0.12))
                    .frame(width: 34, height: 34)
                Image(systemName: status.icon(icon))
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(status == .pending ? Color.secondary : status.fillColor)
                if status == .current {
                    RoundedRectangle(cornerRadius: 10)
                        .strokeBorder(Color.orange.opacity(0.5), lineWidth: 1)
                        .frame(width: 34, height: 34)
                }
            }
            Text(label)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(status.textColor)
        }
        .frame(width: 50)
    }

    /// 进度阶段状态
    private enum ProgressStageStatus {
        case pending, current, done, failed

        var fillColor: Color {
            switch self {
            case .pending: return .secondary.opacity(0.3)
            case .current: return .orange
            case .done: return .green
            case .failed: return .red
            }
        }

        var textColor: Color {
            switch self {
            case .pending: return .secondary
            case .current: return .primary
            case .done: return .primary
            case .failed: return .red
            }
        }

        func icon(_ defaultIcon: String) -> String {
            switch self {
            case .done: return "checkmark"
            case .failed: return "xmark"
            default: return defaultIcon
            }
        }
    }

    /// 读卡器模式隧道状态指示灯颜色
    private var readerTunnelIndicatorColor: Color {
        guard let st = store.readerVoWiFiStatus?.tunnelState else { return .gray }
        switch st {
        case "connected": return .green
        case "connecting", "negotiating": return .orange
        case "error": return .red
        default: return .gray
        }
    }

    /// 读卡器模式隧道状态文字
    private var readerTunnelStatusText: String {
        guard let st = store.readerVoWiFiStatus?.tunnelState else { return "未连接" }
        switch st {
        case "connected": return "隧道已建立"
        case "connecting": return "正在连接…"
        case "negotiating": return "正在协商…"
        case "error": return "连接失败"
        default: return "未连接"
        }
    }

    // MARK: - 组件

    private func sectionTitle(_ title: String) -> some View {
        Text(title).font(.system(size: 16, weight: .semibold))
    }

    private func row(_ key: String, _ value: String, sensitive: Bool = false) -> some View {
        let displayedValue = sensitive ? privateValue(value) : value
        return VStack(alignment: .leading, spacing: 5) {
            Text(key)
                .font(.caption)
                .foregroundStyle(.secondary)
            Button {
                copyValue(value)
            } label: {
                Text(copiedValue == value ? "已复制 ✓" : displayedValue)
                    .font(key == "当前号码" ? .system(size: 21, weight: .semibold, design: .rounded) : .system(size: 13, design: .monospaced))
                    .foregroundStyle(copiedValue == value ? Color.green : Color.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(sensitive && privacyMaskEnabled)
            .help(sensitive && privacyMaskEnabled ? "关闭隐私遮罩后可复制" : "点击复制")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func copyValue(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
        copiedValue = value
		store.notifyCopied()
        Task {
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if copiedValue == value {
                copiedValue = nil
            }
        }
    }

    private func privateValue(_ value: String) -> String {
        PrivacyMask.value(value, enabled: privacyMaskEnabled)
    }

    private func trafficItem(_ title: String, _ value: String) -> some View {
        VStack(spacing: 3) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.headline.monospacedDigit())
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .frame(maxWidth: .infinity)
    }

    private func signalBars(dbm: Int) -> some View {
        let level: Int
        switch dbm {
        case ..<(-100): level = 0
        case ..<(-90): level = 1
        case ..<(-80): level = 2
        case ..<(-65): level = 3
        default: level = 4
        }
        return HStack(alignment: .bottom, spacing: 2) {
            ForEach(0..<4, id: \.self) { i in
                RoundedRectangle(cornerRadius: 1)
                    .fill(i < level ? Color.green : Color.gray.opacity(0.35))
                    .frame(width: 3, height: CGFloat(5 + i * 3))
            }
        }
    }

    private func formatBytes(_ bytes: UInt64?) -> String {
        guard let bytes else { return "-" }
        let formatter = ByteCountFormatter()
        formatter.countStyle = .binary
        formatter.allowedUnits = [.useKB, .useMB, .useGB]
        return formatter.string(fromByteCount: Int64(bytes))
    }

    /// 网卡不可用时显示 "-"，避免后端返回的 0 值造成误导
    private func trafficMetric(_ bytes: UInt64?) -> String {
        guard traffic?.available == true else { return "-" }
        return formatBytes(bytes)
    }

    private var trafficTotal: UInt64? {
        guard let rx = traffic?.rxBytes, let tx = traffic?.txBytes else { return nil }
        return rx + tx
    }

}
