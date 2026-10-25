import SwiftUI

struct SettingsView: View {
    private enum SettingsTab: String, CaseIterable, Identifiable {
        case general = "通用"
        case privacy = "隐私"

        var id: String { rawValue }
    }

    @AppStorage(AppPreferenceKeys.appearance) private var appearance = AppAppearance.system.rawValue
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @AppStorage("silentLaunch") private var silentLaunchEnabled = false
    @AppStorage(MenuBarDisplayOptions.showSignalKey) private var showSignal = false
    @AppStorage(MenuBarDisplayOptions.showDownloadKey) private var showDownload = false
    @AppStorage(MenuBarDisplayOptions.showUploadKey) private var showUpload = false
    @State private var autoLaunchEnabled = false
    @State private var errorMessage: String?
    @State private var selectedTab: SettingsTab = .general
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geometry in
            let viewport = WorkspaceViewport(width: geometry.size.width, height: geometry.size.height)
            settingsContent(in: viewport)
        }
        .frame(minWidth: 560, idealWidth: 640, minHeight: 480, idealHeight: 620)
        .background(WorkspaceColors.canvas)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.16), value: selectedTab)
        .tint(WorkspaceColors.accent)
        .buttonStyle(.proletarius)
        .environment(\.workspaceVisualStyle, .nekoko)
        .onAppear { autoLaunchEnabled = AutoLaunch.isEnabled }
        .onReceive(NotificationCenter.default.publisher(for: AutoLaunch.didChangeNotification)) { _ in
            autoLaunchEnabled = AutoLaunch.isEnabled
        }
        .alert("设置未能保存", isPresented: Binding(
            get: { errorMessage != nil },
            set: { if !$0 { errorMessage = nil } }
        )) {
            Button("好", role: .cancel) { errorMessage = nil }
        } message: {
            Text(errorMessage ?? "未知错误")
        }
    }

    private func settingsContent(in viewport: WorkspaceViewport) -> some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "slider.horizontal.3", tint: WorkspaceColors.accent, size: 36)
                VStack(alignment: .leading, spacing: 4) {
                    Text("PREFERENCES · 设置").font(.system(size: 18, weight: .semibold))
                    Text("调整外观、启动方式与隐私偏好。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, viewport.pageInset)
            .padding(.top, viewport.pageInset)

            Picker("设置分类", selection: $selectedTab) {
                ForEach(SettingsTab.allCases) { tab in
                    Text(tab.rawValue).tag(tab)
                }
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            .frame(width: 230)
            .padding(.horizontal, viewport.pageInset)

            ScrollView {
                Group {
                    switch selectedTab {
                    case .general: generalSettings(in: viewport)
                    case .privacy: privacySettings(in: viewport)
                    }
                }
                .id(selectedTab)
                .transition(.opacity)
                .padding(.horizontal, viewport.pageInset)
                .padding(.bottom, viewport.pageInset)
            }
        }
        .environment(\.workspaceViewport, viewport)
    }

    private func generalSettings(in viewport: WorkspaceViewport) -> some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            settingsSection(title: "外观", symbol: "circle.lefthalf.filled", in: viewport) {
                Picker("外观模式", selection: $appearance) {
                    ForEach(AppAppearance.allCases) { mode in
                        Text(mode.title).tag(mode.rawValue)
                    }
                }
                .labelsHidden()
                .pickerStyle(.segmented)
            }

            settingsSection(title: "启动", symbol: "power", in: viewport) {
                Toggle("登录时启动 Switch", isOn: Binding(
                    get: { autoLaunchEnabled },
                    set: { updateAutoLaunch($0) }))
                Toggle("静默启动", isOn: $silentLaunchEnabled)
                    .help("启动时不展开主窗口，Dock 与菜单栏入口仍可使用")
            }

            settingsSection(title: "菜单栏", symbol: "menubar.rectangle", in: viewport) {
                Toggle("显示信号强度", isOn: $showSignal)
                Toggle("显示下载速率", isOn: $showDownload)
                Toggle("显示上传速率", isOn: $showUpload)
            }
        }
    }

    private func privacySettings(in viewport: WorkspaceViewport) -> some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            settingsSection(title: "敏感信息", symbol: "hand.raised.fill", in: viewport) {
                Toggle("默认启用隐私遮罩", isOn: $privacyMaskEnabled)
                Text("开启后，号码、ICCID、IMEI、序列号和通知内容会被模糊处理。进入短信会话后仍显示消息正文。")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func settingsSection<Content: View>(title: String, symbol: String, in viewport: WorkspaceViewport, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: symbol, tint: WorkspaceColors.accent, size: 34)
                Text(title).font(.system(size: 15, weight: .semibold))
            }
            content()
                .font(.callout)
                .toggleStyle(.switch)
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .workspaceCard()
    }

    private func updateAutoLaunch(_ enabled: Bool) {
        do {
            try AutoLaunch.setEnabled(enabled)
            autoLaunchEnabled = AutoLaunch.isEnabled
        } catch {
            autoLaunchEnabled = AutoLaunch.isEnabled
            errorMessage = error.localizedDescription
        }
    }
}
