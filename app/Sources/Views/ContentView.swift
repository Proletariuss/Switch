import AppKit
import SwiftUI

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @EnvironmentObject private var backend: BackendProcess
    @EnvironmentObject private var store: DashboardStore
    @EnvironmentObject private var smsStore: SMSStore
    @EnvironmentObject private var attentionStore: AttentionStore
    @EnvironmentObject private var updateChecker: UpdateChecker
    @ObservedObject private var mainWindowRequests = MainWindowRequestCenter.shared
    @State private var selection: AppSection? = .activity
    @State private var handledWindowRequest = 0
    @State private var showUpdatePrompt = false
    @State private var hoveredSection: AppSection?
    @AppStorage(AppPreferenceKeys.appearance) private var appearance = AppAppearance.system.rawValue
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    var body: some View {
        NavigationSplitView {
            workspaceSidebar
                .navigationSplitViewColumnWidth(min: 196, ideal: 212, max: 232)
        } detail: {
            Group {
                switch selection ?? .activity {
                case .activity:
                    ActivityView()
                case .home:
                    HomeView(scope: .module)
                case .reader:
                    HomeView(scope: .reader)
                case .multistandby:
                    MultiStandbyView(onOpenSMS: { selectSection(.sms) })
                case .debug:
                    DiagnosticsView()
                default:
                    if backend.state == .running {
                        sectionView
                    } else {
                        ServiceOffView()
                    }
                }
            }
            .proletariusPageBackground()
            .buttonStyle(.proletarius)
            .environment(\.workspaceAccent, (selection ?? .activity).accent)
            .environment(\.workspaceVisualStyle, .nekoko)
            .tint(WorkspaceColors.accent)
            .animation(reduceMotion ? nil : ProletariusMotion.standard, value: selection)
            .workspaceAdaptiveLayout()
        }
        .navigationSplitViewStyle(.balanced)
        .background {
            WorkspaceColors.canvas.ignoresSafeArea()
        }
        .compatibleToolbarRemovingSidebarToggle()
        .toolbar {
            contentViewToolbar
        }
        .compatibleWindowToolbarBackgroundHidden()
        .onAppear {
            smsStore.viewingSMS = selection == .sms
            attentionStore.setViewingCalls(selection == .calls)
            showUpdatePrompt = updateChecker.pendingUpdate != nil
            // 启动即点击通知时，pendingOpenSender 可能早于 onChange 挂载设置，这里补一次
            if smsStore.pendingOpenSender != nil {
                selection = .sms
            }
            handleMainWindowRequest()
        }
        .compatibleOnChange(of: selection) { newValue in
            smsStore.viewingSMS = newValue == .sms
            attentionStore.setViewingCalls(newValue == .calls)
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            if selection == .calls {
                attentionStore.markCallsViewed()
            }
        }
        .compatibleOnChange(of: updateChecker.pendingUpdate != nil) { shown in
            showUpdatePrompt = shown
        }
        .alert(
            "发现新版本",
            isPresented: $showUpdatePrompt,
            presenting: updateChecker.pendingUpdate
        ) { release in
            Button("前往更新") {
                if let url = updateChecker.downloadURL {
                    NSWorkspace.shared.open(url)
                }
                updateChecker.dismissUpdate()
            }
            .keyboardShortcut(.defaultAction)
            Button("暂不更新", role: .cancel) {
                updateChecker.dismissUpdate()
            }
            Button("跳过本次更新") {
                updateChecker.skipUpdate(release)
            }
        } message: { release in
            Text("当前版本 \(updateChecker.currentVersion)，新版本 \(release.tagName) 已发布。")
        }
        .compatibleOnChange(of: smsStore.pendingOpenSender) { sender in
            // 点击短信通知：切到短信页（号码选择由短信页处理）
            if sender != nil {
                selection = .sms
            }
        }
        .compatibleOnChange(of: mainWindowRequests.generation) { _ in
            handleMainWindowRequest()
        }
    }

    private var workspaceSidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                SwitchBrandLogo(placement: .sidebar)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Switch").font(.system(size: 15, weight: .bold))
                    Text("通信与卡片工作台").font(.system(size: 10)).foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 18)
            .padding(.top, 20)
            .padding(.bottom, 22)

            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    workspaceNavigationGroup("概览", items: [.activity])
                    workspaceNavigationGroup("设备", items: [.home, .reader])
                    workspaceNavigationGroup("通信", items: [.multistandby, .sms, .calls])
                    workspaceNavigationGroup("工具", items: [.debug])
                }
                .padding(.horizontal, 12)
                .padding(.bottom, 20)
            }
            .scrollIndicators(.hidden)

            HStack(spacing: 7) {
                Circle().fill(backend.state == .running ? Color.green : Color.secondary)
                    .frame(width: 5, height: 5)
                Text(backend.state == .running ? "服务运行中" : "服务待就绪")
                    .font(.system(size: 11)).foregroundStyle(.secondary)
                Spacer(minLength: 0)
                Text(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "")
                    .font(.system(size: 10, design: .monospaced)).foregroundStyle(.tertiary)
            }
            .padding(18)
        }
        .background(WorkspaceColors.sidebar)
    }

    private func workspaceNavigationGroup(_ title: String, items: [AppSection]) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
            ForEach(items) { section in
                let selected = selection == section
                Button {
                    selectSection(section)
                } label: {
                    HStack(spacing: 12) {
                        workspaceNavigationIcon(for: section, selected: selected)
                            .frame(width: 20, height: 20)
                            .accessibilityHidden(true)
                        Text(section.rawValue)
                            .font(.system(size: 13, weight: selected ? .semibold : .medium))
                            .lineLimit(1)
                        Spacer(minLength: 0)
                        if let count = attentionCount(for: section), count > 0 {
                            AttentionBadge(count: count, accessibilityName: section == .sms ? "未读短信" : "未查看电话")
                        }
                    }
                    .foregroundStyle(selected ? WorkspaceColors.accent : Color.primary)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 11)
                    .background {
                        RoundedRectangle(cornerRadius: 11)
                            .fill(WorkspaceColors.accent.opacity(selected ? 0.11 : (hoveredSection == section ? 0.045 : 0)))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .onHover { hoveredSection = $0 ? section : nil }
                .accessibilityLabel(section.rawValue)
                .accessibilityValue(selected ? "当前页面" : "")
            }
        }
    }

    @ViewBuilder
    private func workspaceNavigationIcon(for section: AppSection, selected: Bool) -> some View {
        if section == .multistandby {
            Image("NWaitLoop")
                .resizable()
                .interpolation(.high)
                .scaledToFit()
        } else {
            Image(systemName: section.icon)
                .font(.system(size: 15, weight: .medium))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(selected ? WorkspaceColors.accent : section.accent)
        }
    }

    /// 隐私遮罩只出现在会呈现号码或设备标识的工作区。活动、诊断和关于页
    /// 不改变敏感字段的展示，也不占用工具栏空间。
    private var showsPrivacyControl: Bool {
        switch selection ?? .activity {
        case .home, .reader, .multistandby, .sms, .calls:
            return true
        case .activity, .debug:
            return false
        }
    }

    @ToolbarContentBuilder
    private var contentViewToolbar: some ToolbarContent {
        if showsPrivacyControl {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    privacyMaskEnabled.toggle()
                } label: {
                    PrivacyMaskControlMark(isMasked: privacyMaskEnabled)
                }
                .buttonStyle(.proletariusToolbar)
                .help(privacyMaskEnabled ? "隐私内容已遮盖（点击显示）" : "隐私内容可见（点击遮盖）")
                .accessibilityLabel(privacyMaskEnabled ? "显示隐私内容" : "遮盖隐私内容")
                .accessibilityValue(privacyMaskEnabled ? "已遮盖" : "可见")
            }
            .compatibleSharedToolbarBackgroundHidden()

            if #available(macOS 26.0, *) {
                ToolbarSpacer(.fixed, placement: .primaryAction)
            }
        }
        ToolbarItem(placement: .primaryAction) {
            appearanceMenu
        }
        .compatibleSharedToolbarBackgroundHidden()
    }

    private var appearanceMode: AppAppearance {
        AppAppearance(rawValue: appearance) ?? .system
    }

    private var appearanceMenu: some View {
        Button {
            cycleAppearance()
        } label: {
                Image(systemName: appearanceIcon)
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.primary)
                    .frame(width: ProletariusLayout.toolbarButtonFrame,
                           height: ProletariusLayout.toolbarButtonFrame)
        }
        .buttonStyle(.proletariusToolbar)
        .help("外观：\(appearanceMode.title)；点击切换")
        .accessibilityLabel("外观模式")
        .accessibilityValue(appearanceMode.title)
    }

    private func cycleAppearance() {
        let next: AppAppearance
        switch appearanceMode {
        case .system: next = .light
        case .light: next = .dark
        case .dark: next = .system
        }
        appearance = next.rawValue
    }

    private var appearanceIcon: String {
        switch appearanceMode {
        case .system: return colorScheme == .dark ? "moon.fill" : "sun.max.fill"
        case .light: return "sun.max.fill"
        case .dark: return "moon.fill"
        }
    }

    @ViewBuilder
    private var sectionView: some View {
        switch selection ?? .activity {
        case .activity: ActivityView()
        case .home: HomeView(scope: .module)
        case .reader: HomeView(scope: .reader)
        case .multistandby: MultiStandbyView(onOpenSMS: { selectSection(.sms) })
        case .sms: SMSView()
        case .calls: CallView()
        case .debug: DiagnosticsView()
        }
    }

    private func attentionCount(for section: AppSection) -> Int? {
        switch section {
        case .calls:
            return attentionStore.unviewedCallCount
        case .sms:
            return attentionStore.unreadSMSCount
        default:
            return nil
        }
    }

    private func selectSection(_ section: AppSection) {
        selection = section
        NSApp.activate(ignoringOtherApps: true)
        if section == .calls {
            attentionStore.markCallsViewed()
        }
    }

    private func handleMainWindowRequest() {
        guard handledWindowRequest != mainWindowRequests.generation else { return }
        handledWindowRequest = mainWindowRequests.generation
        guard let destination = mainWindowRequests.destination else { return }
        selection = destination
        if destination == .calls {
            attentionStore.markCallsViewed()
        }
    }
}
