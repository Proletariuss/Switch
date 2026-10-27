import AppKit
import SwiftUI
import UserNotifications
import UniformTypeIdentifiers

/// 调试与诊断：按功能分页组织（网络诊断 / AT 调试 / 通知调试），
/// 后续新增调试功能时新增一个 tab 及对应子视图即可，互不影响。
struct DiagnosticsView: View {
    enum DebugTab: String, CaseIterable, Identifiable {
        case support = "运行日志"
        case network = "网络诊断"
        case at = "AT 调试"
        case notify = "通知调试"

        var id: String { rawValue }
    }

    @State private var tab: DebugTab = .support
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.workspaceViewport) private var viewport

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 20) {
                    diagnosticsHeading
                        .frame(minWidth: 360)
                    diagnosticsPicker.frame(width: 440)
                }
                VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                    diagnosticsHeading
                    diagnosticsPicker.frame(maxWidth: 480)
                }
            }
            .padding(.horizontal, viewport.pageInset)
            .padding(.top, viewport.pageInset)

            ModuleRecoveryStatusView()
                .padding(.horizontal, viewport.pageInset)

            Group {
                switch tab {
                case .network: DiagnosticNetworkWorkspaceView()
                case .at: DiagnosticATView()
                case .notify: DiagnosticNotifyView()
                case .support: SupportDiagnosticsView()
                }
            }
            .id(tab)
            .transition(.opacity)
            .padding(.horizontal, viewport.pageInset)
            .padding(.bottom, viewport.pageInset)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.16), value: tab)
        .buttonStyle(.proletarius)
        .tint(WorkspaceColors.accent)
    }

    @ViewBuilder
    private var diagnosticsHeading: some View {
        if viewport.isCompactHeight {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "stethoscope", tint: WorkspaceColors.accent,
                                size: WorkspaceHeaderMetrics.iconFrame)
                VStack(alignment: .leading, spacing: 4) {
                    Text("DEVELOPER TOOLS · 调试与诊断")
                        .font(.system(size: 17, weight: .semibold))
                    Text("实时运行日志、网络验证、AT 指令与通知测试。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
        } else {
            WorkspaceHero(title: "调试与诊断", subtitle: "实时运行日志、网络验证、AT 指令与通知测试。", symbol: "stethoscope", eyebrow: "DEVELOPER TOOLS")
        }
    }

    private var diagnosticsPicker: some View {
        Picker("诊断工具", selection: $tab) {
            ForEach(DebugTab.allCases) { item in
                Text(item.rawValue).tag(item)
            }
        }
        .labelsHidden()
        .pickerStyle(.segmented)
    }
}

// MARK: - 网络诊断

private struct DiagnosticNetworkWorkspaceView: View {
    @State private var proxy = false
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Picker("网络工具", selection: $proxy) {
                Text("模块网络").tag(false)
                Text("代理 UDP").tag(true)
            }.labelsHidden().pickerStyle(.segmented).frame(maxWidth: 260)
            Group {
                if proxy { UDPProxyDiagnosticView() }
                else { DiagnosticNetworkView() }
            }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}

struct DiagnosticNetworkView: View {
    @EnvironmentObject private var backend: BackendProcess
    @EnvironmentObject private var store: DashboardStore
    @Environment(\.workspaceViewport) private var viewport

    @State private var diagnostic: NetworkDiagnostic?
    @State private var diagnosticError: String?
    @State private var isLoading = false
    @State private var collectionTask: Task<Void, Never>?
    @State private var collectionRevision: UInt64 = 0
    @State private var capturedAt: Date?
    @State private var exportFeedback: String?

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "network", tint: WorkspaceColors.accent, size: 40)
                WorkspaceSectionHeader(title: "连接与网络", detail: "模块、系统接口与路由的当前信息。")
                if let diagnostic, let errors = diagnostic.errors, !errors.isEmpty {
                    Label("部分命令失败", systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                Spacer()
                Button { exportSummary() } label: {
                    Label("导出摘要", systemImage: "square.and.arrow.up")
                }.controlSize(.small)
                .help("仅导出状态与采集元数据，不包含短信、号码、密钥或原始日志")
                if isLoading {
                    Button("停止采集") { cancelCollection() }.controlSize(.small)
                }
                Button {
                    loadDiagnostic()
                } label: {
                    Label(isLoading ? "采集中" : "重新采集", systemImage: "arrow.clockwise")
                }
                .controlSize(.small)
                .disabled(isLoading)
                .help("重新采集诊断信息")
            }
            if let capturedAt {
                Text("采集于 \(capturedAt.formatted(date: .omitted, time: .standard))"
                     + (diagnostic?.partial == true ? " · 部分结果" : "")
                     + (diagnostic?.durationMS.map { " · \($0) ms" } ?? ""))
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let diagnosticError {
                Text(diagnosticError).font(.caption).foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let exportFeedback {
                Text(exportFeedback).font(.caption).foregroundStyle(.secondary)
            }
            Divider()
            ScrollView {
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: viewport.columnCount(minimumWidth: 270, maximum: 3)), alignment: .leading, spacing: 10) {
                    if let diagnostic {
                        detailRows(for: diagnostic)
                    } else if let diagnosticError {
                        Label(isModuleDisconnectedError(diagnosticError) ? moduleDisconnectedFriendlyMessage : diagnosticError, systemImage: "exclamationmark.circle")
                            .font(.callout).foregroundStyle(.secondary)
                            .padding(.vertical, 16)
                    } else {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("读取诊断信息…").foregroundStyle(.secondary)
                        }
                    }
                }
                .font(.callout)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollIndicators(.visible)
        }
        .padding(viewport.cardPadding)
        .workspaceCard()
        .onAppear { loadDiagnostic() }
        .onDisappear { cancelCollection() }
        .compatibleOnChange(of: backend.state) { state in
            if state != .running {
                cancelCollection()
                diagnosticError = "后端未运行，当前内容为上次采集结果。"
            }
        }
    }

    private func detailRows(for diagnostic: NetworkDiagnostic) -> some View {
        Group {
            detailRow("USB 网卡模式", diagnostic.usbnetMode ?? "-")
            if let usbcfg = diagnostic.usbcfg {
                detailRow("USB 配置", usbcfg)
            }
            if let present = diagnostic.usbNetworkPresent {
                detailRow("USB 网络接口", present ? "存在" : "不存在")
            }
            if let route = diagnostic.defaultRoute {
                detailRow("默认出口", "\(route.interface ?? "-") → \(route.gateway ?? "-")")
            }
            ForEach(diagnostic.macInterfaces ?? [], id: \.name) { iface in
                detailRow("网络接口", "\(iface.name ?? "-")（\(iface.kind ?? "-")，\(iface.status ?? "-")）\(iface.ipv4 ?? "")")
            }
            ForEach(diagnostic.pdpContexts ?? [], id: \.id) { ctx in
                detailRow("PDP 上下文 \(ctx.id ?? 0)", "\(ctx.pdn ?? "-") \(ctx.apn ?? "-")")
            }
            if let addresses = diagnostic.pdpAddresses, !addresses.isEmpty {
                detailRow("PDP 地址", addresses.joined(separator: "、"))
            }
            if let errors = diagnostic.errors, !errors.isEmpty {
                ForEach(errors.sorted(by: { $0.key < $1.key }), id: \.key) { key, value in
                    detailRow("\(key) 错误", value)
                }
            }
        }
    }

    private func detailRow(_ key: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(key).font(.caption).foregroundStyle(.secondary)
            Text(value)
                .font(.system(size: 12, design: .monospaced))
                .copyableInformation(value)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(viewport.isCompactHeight ? 10 : 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 10))
    }

    private func loadDiagnostic() {
        guard !isLoading else { return }
        guard case .running = backend.state else {
            diagnosticError = "后端未运行，保留上次采集结果；启动服务后可重新采集。"
            return
        }
        isLoading = true
        collectionRevision &+= 1
        let revision = collectionRevision
        diagnosticError = nil
        collectionTask = Task {
            do {
                let diag: NetworkDiagnostic = try await APIClient(timeoutInterval: 25).get("api/network")
                guard !Task.isCancelled, revision == collectionRevision else { return }
                await MainActor.run {
                    diagnostic = diag
                    diagnosticError = nil
                    isLoading = false
                    collectionTask = nil
                    capturedAt = diag.capturedAtMS.map { Date(timeIntervalSince1970: Double($0) / 1_000) } ?? Date()
                }
            } catch {
                guard !Task.isCancelled, revision == collectionRevision else { return }
                await MainActor.run {
                    diagnosticError = error.localizedDescription
                    isLoading = false
                    collectionTask = nil
                }
            }
        }
    }

    private func cancelCollection() {
        collectionRevision &+= 1
        collectionTask?.cancel()
        collectionTask = nil
        if isLoading { diagnosticError = "采集已停止，保留上次完成的结果。" }
        isLoading = false
    }

    private func exportSummary() {
        let panel = NSSavePanel()
        panel.title = "导出脱敏诊断摘要"
        panel.nameFieldStringValue = "Switch-diagnostic-summary.json"
        panel.allowedContentTypes = [.json]
        panel.message = "包含版本、状态与采集元数据；不包含短信正文、号码、网络地址、凭据或原始日志。"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let state: String
            switch backend.state {
            case .running: state = "running"
            case .starting: state = "starting"
            case .stopping: state = "stopping"
            case .stopped: state = "stopped"
            case .failed: state = "failed"
            }
            let summary = SafeDiagnosticSummary(
                version: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown",
                build: Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "unknown",
                backendState: state,
                lastExit: backend.lastExit.map { .init(status: $0.status, signaled: $0.signaled, expected: $0.expected) },
                moduleSnapshotStale: store.statusStale,
                readerConnected: store.readerConnected,
                moduleSMSReady: store.voWiFiStatus?.smsReady == true,
                readerSMSReady: store.readerVoWiFiStatus?.smsReady == true,
                collectedAt: capturedAt,
                network: diagnostic.map { .init(partial: $0.partial == true,
                    durationMS: $0.durationMS, completedCommands: $0.completedCommands ?? 0,
                    failedCommands: Array(($0.errors ?? [:]).keys).sorted(),
                    usbNetworkPresent: $0.usbNetworkPresent, interfaceCount: $0.macInterfaces?.count ?? 0) })
            try summary.data().write(to: url, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            exportFeedback = "诊断摘要已保存。"
        } catch {
            exportFeedback = "导出失败：\(error.localizedDescription)"
        }
    }
}

// MARK: - AT 调试

struct DiagnosticATView: View {
    @State private var command = ""
    @State private var history: [ATEntry] = []
    @State private var atBusy = false
    @State private var atError: String?
    @State private var atTask: Task<Void, Never>?
    @State private var atRevision: UInt64 = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.workspaceViewport) private var viewport

    private let quickCommands = ["AT", "AT+CSQ", "AT+COPS?", "AT+CPIN?", "AT+CNUM", "AT+QCFG=\"usbnet\""]

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "terminal", tint: WorkspaceColors.accent, size: 40)
                WorkspaceSectionHeader(title: "AT 控制台", detail: "单行命令，最多 4096 字节；支持组合命令，模块响应会与通道故障分别显示。")
            }
            quickBar
            if let atError {
                Label {
                    Text(isModuleDisconnectedError(atError) ? moduleDisconnectedFriendlyMessage : atError)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                } icon: {
                    Image(systemName: "exclamationmark.triangle")
                }
                .font(.caption)
                .foregroundStyle(.orange)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Divider()
            outputArea
            inputBar
            ForEach(ATRequest(command: command).impactDescriptions, id: \.self) { impact in
                Label(impact, systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(viewport.cardPadding)
        .workspaceCard()
        .onDisappear { cancelATWait() }
    }

    private var quickBar: some View {
        ScrollView(.horizontal, showsIndicators: true) {
            HStack(spacing: 8) {
                ForEach(quickCommands, id: \.self) { cmd in
                    Button(cmd) {
                        command = cmd
                    }
                    .buttonStyle(.proletarius)
                    .controlSize(.small)
                }
            }
            .padding(.vertical, 2)
        }
        .accessibilityLabel("快捷指令")
    }

    private var outputArea: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if history.isEmpty {
                        VStack(spacing: 8) {
                            WorkspaceSymbol(systemName: "terminal", tint: WorkspaceColors.accent, size: 54)
                            Text("输入 AT 指令并发送，响应将显示在这里")
                                .font(.callout).foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, viewport.isCompactHeight ? 20 : 42)
                    } else {
                        ForEach(history) { entry in
                            ATEntryRow(entry: entry)
                                .id(entry.id)
                        }
                    }
                }
                .padding(16)
            }
            .scrollIndicators(.visible)
            .compatibleOnChange(of: history.count) { _ in
                if let last = history.last {
                    withAnimation(reduceMotion ? nil : .easeOut(duration: 0.18)) {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }
        }
        .frame(minHeight: viewport.isVeryShort ? 80 : 140)
        .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))
    }

    private var inputBar: some View {
        HStack(spacing: 10) {
            TextField("例如：AT+CSQ", text: $command)
                .textFieldStyle(.workspace)
                .font(.monospaced(.body)())
                .onSubmit { sendAT() }
            Button {
                sendAT()
            } label: {
                if atBusy {
                    ProgressView().controlSize(.small)
                } else {
                    Label("发送", systemImage: "paperplane")
                }
            }
            .buttonStyle(.proletariusPrimary)
            .disabled(atBusy || command.isEmpty)
        }
        .padding(.top, 2)
    }

    private func sendAT() {
        // Preserve pasted control characters so the backend rejects them;
        // trimming newlines here could silently turn multiline input valid.
        let cmd = command.trimmingCharacters(in: CharacterSet(charactersIn: " "))
        // Return in the text field bypasses the button's disabled state.
        // Keep one diagnostic command in flight regardless of input method.
        guard !cmd.isEmpty, !atBusy else { return }
        atBusy = true
        atRevision &+= 1
        let revision = atRevision
        let client = APIClient()
        atTask = Task {
            do {
                let result: ATResult = try await client.send(
                    "api/at", body: ATRequest(command: cmd))
                guard !Task.isCancelled, revision == atRevision else { return }
                await MainActor.run {
                    history.append(ATEntry(command: cmd, response: result.response ?? "", isError: result.moduleFailureDescription != nil))
                    atBusy = false
                    atError = result.moduleFailureDescription
                    atTask = nil
                }
            } catch {
                guard !Task.isCancelled, revision == atRevision else { return }
                await MainActor.run {
                    let errDesc = error.localizedDescription
                    let displayMsg = isModuleDisconnectedError(errDesc) ? moduleDisconnectedFriendlyMessage : "错误：\(errDesc)"
                    history.append(ATEntry(command: cmd, response: displayMsg, isError: true))
                    atBusy = false
                    atError = errDesc
                    atTask = nil
                }
            }
        }
        command = ""
    }

    private func cancelATWait() {
        atRevision &+= 1
        atTask?.cancel()
        atTask = nil
        atBusy = false
        // Cancellation stops waiting for a response; an AT command already
        // submitted to the module is not rolled back or automatically retried.
    }
}

// MARK: - 通知调试

struct DiagnosticNotifyView: View {
    @EnvironmentObject private var smsStore: SMSStore
    @EnvironmentObject private var store: DashboardStore
    @EnvironmentObject private var attentionStore: AttentionStore
    @Environment(\.workspaceViewport) private var viewport

    @State private var notifyAuth: String?
    @State private var notifyFeedback: String?
    /// 已授权但通知样式被系统设为"无"（静默，不弹横幅）
    @State private var notifyBannerOff = false

    var body: some View {
        ScrollView {
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: viewport.sectionSpacing, alignment: .top), count: viewport.columnCount(minimumWidth: 260, maximum: 2)), alignment: .leading, spacing: viewport.sectionSpacing) {
                VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                    HStack(spacing: 12) {
                        WorkspaceSymbol(systemName: "bell.badge", tint: WorkspaceColors.accent, size: 40)
                        WorkspaceSectionHeader(title: "通知测试", detail: "验证系统短信通知与应用来电提醒。")
                    }
                    HStack(spacing: 12) {
                        Text("通知权限").font(.callout).foregroundStyle(.secondary)
                        Spacer()
                        if let notifyAuth {
                            Text(notifyAuth)
                                .font(.callout.weight(.semibold))
                                .foregroundStyle(notifyAuth == "已开启" ? Color.green : Color.orange)
                        } else {
                            ProgressView().controlSize(.small)
                        }
                        if notifyBannerOff {
                            Button("打开通知设置") { openNotifySettings() }
                                .controlSize(.small)
                                .help("在系统设置中将通知样式改为横幅，通知才会弹出")
                        }
                    }
                    .padding(14)
                    .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))

                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), spacing: 12)], spacing: 12) {
                        Button { simulateNotify() } label: {
                            Label("模拟短信通知", systemImage: "bell.badge")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.proletarius)
                        .help("直接发送一条测试通知，验证新短信系统通知是否正常工作")

                        Button { simulateCallNotify() } label: {
                            Label("模拟来电通知", systemImage: "phone.fill")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.proletarius)
                        .help("直接弹出一张自定义来电卡片，验证来电提醒是否正常工作")
                    }

                    if let notifyFeedback {
                        Text(notifyFeedback)
                            .font(.caption)
                            .foregroundStyle(notifyFeedback.contains("成功") || notifyFeedback.contains("已发送") ? Color.secondary : Color.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(viewport.cardPadding)
                .workspaceCard()

                VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                    HStack(spacing: 12) {
                        WorkspaceSymbol(systemName: "number.circle", tint: WorkspaceColors.accent, size: 40)
                        WorkspaceSectionHeader(title: "未读计数调试", detail: "调整测试计数，不创建真实短信或通话。")
                        Button("计数清零") { attentionStore.clearForDebug() }
                            .controlSize(.small)
                            .disabled(!attentionStore.hasAttention)
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 220), spacing: 12)], spacing: 12) {
                        Button { attentionStore.incrementUnreadSMSForDebug() } label: {
                            HStack(spacing: 8) {
                                Text("增加未读短信")
                                Spacer(minLength: 0)
                                if attentionStore.unreadSMSCount > 0 {
                                    AttentionBadge(count: attentionStore.unreadSMSCount, accessibilityName: "未读短信")
                                }
                            }
                            .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.proletarius)
                        .help("将未读短信计数增加 1，不创建真实短信")

                        Button { attentionStore.incrementUnviewedCallForDebug() } label: {
                            HStack(spacing: 8) {
                                Text("增加未查看电话")
                                Spacer(minLength: 0)
                                if attentionStore.unviewedCallCount > 0 {
                                    AttentionBadge(count: attentionStore.unviewedCallCount, accessibilityName: "未查看电话")
                                }
                            }
                            .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.proletarius)
                        .help("将未查看电话计数增加 1，不创建真实通话记录")
                    }
                }
                .padding(viewport.cardPadding)
                .workspaceCard()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .onAppear { loadNotifyAuth() }
    }

    private func loadNotifyAuth() {
        guard !AcceptanceBootstrap.isAcceptance else {
            notifyAuth = "隔离验收中不可用"
            notifyBannerOff = false
            return
        }
        Task {
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            let text: String
            switch settings.authorizationStatus {
            case .authorized, .provisional, .ephemeral: text = "已开启"
            case .notDetermined: text = "未授权"
            case .denied: text = "已拒绝"
            @unknown default: text = "未知"
            }
            await MainActor.run {
                notifyAuth = text
                notifyBannerOff = settings.authorizationStatus == .authorized && settings.alertSetting == .disabled
            }
        }
    }

    private func openNotifySettings() {
        // 打开 macOS 系统设置的通知面板。
        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }

    private func simulateNotify() {
        Task {
            notifyFeedback = "正在发送…"
            notifyFeedback = await smsStore.simulateNotification()
            loadNotifyAuth()
        }
    }

    private func simulateCallNotify() {
        notifyFeedback = "已弹出模拟来电卡片（30 秒后自动收起）"
        IncomingCallCard.shared.show(store: store, preview: true)
    }
}

// MARK: - AT 记录

struct ATEntry: Identifiable {
    let id = UUID()
    let command: String
    let response: String
    var isError: Bool = false
}

struct ATEntryRow: View {
    let entry: ATEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(entry.command)
                .font(.monospaced(.body)())
                .textSelection(.enabled)
            Text(entry.response.isEmpty ? "(无响应)" : entry.response)
                .font(.monospaced(.body)())
                .foregroundStyle(entry.isError ? Color.red : Color(nsColor: .secondaryLabelColor))
                .textSelection(.enabled)
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(entry.isError ? Color.red.opacity(0.08) : WorkspaceColors.accent.opacity(0.05)))
        .padding(.vertical, 3)
    }
}
