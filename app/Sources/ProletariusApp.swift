import AppKit
import Combine
import SwiftUI
import UserNotifications

private enum AppLaunchMode {
    static var shouldLaunchSilently: Bool {
        CommandLine.arguments.contains("--background")
            || UserDefaults.standard.bool(forKey: "silentLaunch")
    }
}

@MainActor
final class MainWindowRequestCenter: ObservableObject {
    static let shared = MainWindowRequestCenter()

    @Published private(set) var generation = 0
    @Published private(set) var destination: AppSection?

    private init() {}

    func requestOpen(destination: AppSection? = nil) {
        self.destination = destination
        generation &+= 1
    }
}

@MainActor
private final class AppDependencies: ObservableObject {
    let backend: BackendProcess
    let store: DashboardStore
    let smsStore: SMSStore
    let multiStandbyStore: MultiStandbyStore
    let attentionStore: AttentionStore
    let updateChecker: UpdateChecker
    private var diagnosticLifecycle: AnyCancellable?

    init(context: AcceptanceContext?) {
        let backend = BackendProcess.shared
        self.backend = backend
        ClientDiagnostics.shared.start(root: SwitchRuntimePaths.dataDirectory())
        if context == nil { ReaderBLEBridge.shared.start(socketPath: backend.bleSocketPath) }
        store = DashboardStore(backend: backend)
        smsStore = .shared
        multiStandbyStore = .shared
        attentionStore = .shared
        updateChecker = .shared
        diagnosticLifecycle = backend.$state.sink { state in
            let stage: String
            switch state {
            case .stopped: stage = "stopped"
            case .starting: stage = "starting"
            case .running: stage = "running"
            case .stopping: stage = "stopping"
            case .failed: stage = "failed"
            }
            ClientDiagnostics.shared.lifecycle(component: "backend", stage: stage)
        }
    }
}

struct ProletariusApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var dependencies: AppDependencies
    private let launchSilently = AppLaunchMode.shouldLaunchSilently

    init() {
        // SwitchEntry has already validated the immutable context before any
        // wrapper/delegate. Keep this explicit ordering at the dependency seam.
        _dependencies = StateObject(wrappedValue: AppDependencies(context: AcceptanceBootstrap.context))
    }

    var body: some Scene {
        Window("Switch", id: AppSceneID.mainWindow) {
            MainAppContent(appDelegate: appDelegate, dependencies: dependencies)
        }
        .windowStyle(.titleBar)
        .windowToolbarStyle(.unified)
        .defaultSize(width: 1180, height: 760)
        .defaultLaunchBehavior(launchSilently ? .suppressed : .presented)
        .commands {
            SidebarCommands()
            ProletariusNavigationCommands()
        }

        Settings {
            SettingsView()
        }

        ProletariusMenuBarScene(appDelegate: appDelegate, dependencies: dependencies)
    }
}

private struct ProletariusMenuBarScene: Scene {
    let appDelegate: AppDelegate
    let dependencies: AppDependencies

    var body: some Scene {
        MenuBarExtra {
            MenuBarDashboardPanel(
                appDelegate: appDelegate,
                backend: dependencies.backend,
                store: dependencies.store,
                smsStore: dependencies.smsStore,
                attentionStore: dependencies.attentionStore,
                updateChecker: dependencies.updateChecker)
        } label: {
            MenuBarStatusLabel(
                appDelegate: appDelegate,
                attentionStore: dependencies.attentionStore,
                store: dependencies.store)
        }
        .menuBarExtraStyle(.window)
    }
}

private struct MainAppContent: View {
    let appDelegate: AppDelegate
    let dependencies: AppDependencies
    @AppStorage(AppPreferenceKeys.appearance) private var appearance = AppAppearance.system.rawValue

    var body: some View {
        ContentView()
            .environmentObject(dependencies.backend)
            .environmentObject(dependencies.store)
            .environmentObject(dependencies.smsStore)
            .environmentObject(dependencies.multiStandbyStore)
            .environmentObject(dependencies.attentionStore)
            .environmentObject(dependencies.updateChecker)
            .frame(minWidth: 920, minHeight: 620)
            .background(MainWindowRegistration(onResolve: appDelegate.registerMainWindow))
            .onAppear {
                AppAppearance.apply(appearance)
                appDelegate.bindDashboardStore(dependencies.store)
            }
            .compatibleOnChange(of: appearance) { value in
                AppAppearance.apply(value)
            }
    }
}

/// Register only the main scene. Settings and incoming-call panels must never
/// become the window restored by Dock or menu-bar actions.
private struct MainWindowRegistration: NSViewRepresentable {
    let onResolve: (NSWindow) -> Void

    func makeNSView(context: Context) -> WindowProbe {
        WindowProbe(onResolve: onResolve)
    }

    func updateNSView(_ nsView: WindowProbe, context: Context) {
        nsView.onResolve = onResolve
    }

    final class WindowProbe: NSView {
        var onResolve: (NSWindow) -> Void

        init(onResolve: @escaping (NSWindow) -> Void) {
            self.onResolve = onResolve
            super.init(frame: .zero)
        }

        required init?(coder: NSCoder) { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let window { onResolve(window) }
        }
    }
}

private struct ProletariusNavigationCommands: Commands {
    var body: some Commands {
        CommandMenu("导航") {
            navigationButton("活动", destination: .activity, key: "1")
            navigationButton("模块", destination: .home, key: "2")
            navigationButton("读卡器", destination: .reader, key: "3")
            navigationButton("N待", destination: .multistandby, key: "8")
            navigationButton("短信中心", destination: .sms, key: "4")
            navigationButton("通话", destination: .calls, key: "5")
            navigationButton("调试与诊断", destination: .debug, key: "6")
        }
    }

    private func navigationButton(_ title: String, destination: AppSection, key: KeyEquivalent) -> some View {
        Button(title) {
            MainWindowRequestCenter.shared.requestOpen(destination: destination)
        }
        .keyboardShortcut(key, modifiers: .command)
    }
}

@MainActor
final class AppDelegate: NSObject, ObservableObject, NSApplicationDelegate,
    UNUserNotificationCenterDelegate
{
    @Published private(set) var menuBarPresentation = MenuBarPresentation.loading
    private weak var dashboardStore: DashboardStore?
    private var statusCancellables = Set<AnyCancellable>()
    private var previousTrafficSample: TrafficCounterSample?
    private var downloadBytesPerSecond: Double?
    private var uploadBytesPerSecond: Double?
    private var signalImageCache: [Int: NSImage] = [:]
    private let mainWindowCoordinator = MainWindowCoordinator()
    private let backendTermination = BackendAppTermination()
    func applicationWillFinishLaunching(_ notification: Notification) {
        AppAppearance.apply(
            UserDefaults.standard.string(forKey: AppPreferenceKeys.appearance)
                ?? AppAppearance.system.rawValue)
        NSApp.setActivationPolicy(.regular)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        URLProtocol.registerClass(UnixSocketURLProtocol.self)
        // 通知权限按需请求（收到短信时弹出系统授权框），启动时不自动注册
        if !AcceptanceBootstrap.isAcceptance { UNUserNotificationCenter.current().delegate = self }
        // Forwarding must restore its Keychain-backed configuration even on a
        // silent menu-bar launch where SMSView is never constructed. Install
        // the backend-state subscriber before starting the daemon, so both
        // this launch and later daemon restarts reconfigure the Rust worker.
        if !AcceptanceBootstrap.isAcceptance { _ = SMSForwardingStore.shared }
        // 打开应用即自动启动后端，退出应用时自动停止
        BackendProcess.shared.start()
        if !AcceptanceBootstrap.isAcceptance {
            Task { await UpdateChecker.shared.autoCheckIfNeeded() }
        }

        // Dock and the persistent menu-bar entry coexist; closing the window
        // keeps the app and its owned communication sessions running.
        NSApp.setActivationPolicy(.regular)

        // 首次启动默认开启开机自启
        let defaults = UserDefaults.standard
        if !AcceptanceBootstrap.isAcceptance && !defaults.bool(forKey: "autoLaunchConfigured") {
            defaults.set(true, forKey: "autoLaunchConfigured")
            try? AutoLaunch.setEnabled(true)
        }

        // The scene honors silent launch; a normal launch activates the window.
        if !AppLaunchMode.shouldLaunchSilently {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.showMainWindow()
            }
        }

        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleMainWindowBecameKey(_:)),
            name: NSWindow.didBecomeKeyNotification,
            object: nil)
    }

    @objc private func handleMainWindowBecameKey(_ note: Notification) {
        guard let window = note.object as? NSWindow,
              window.identifier?.rawValue == AppSceneID.mainWindow else { return }
        registerMainWindow(window)
    }

    fileprivate func registerMainWindow(_ window: NSWindow) {
        mainWindowCoordinator.register(window)
    }

    // MARK: - 菜单栏状态

    /// 将菜单栏绑定到首页已有的 2 秒状态轮询，不额外请求后端。
    func bindDashboardStore(_ store: DashboardStore) {
        guard dashboardStore !== store else { return }
        dashboardStore = store
        statusCancellables.removeAll()
        resetTrafficRate()

        store.deviceSnapshot.$state
            .receive(on: RunLoop.main)
            .sink { [weak self, weak store] moduleState in
                guard let self else { return }
                if moduleState.isStale {
                    self.resetTrafficRate()
                }
                self.renderMenuBar(
                    status: moduleState.status,
                    traffic: store?.traffic,
                    isStale: moduleState.isStale)
            }
            .store(in: &statusCancellables)

        store.moduleNetwork.$traffic
            .receive(on: RunLoop.main)
            .sink { [weak self, weak store] traffic in
                guard let self else { return }
                self.consumeTrafficSample(traffic)
                self.renderMenuBar(
                    status: store?.status,
                    traffic: traffic,
                    isStale: store?.statusStale ?? false)
            }
            .store(in: &statusCancellables)

        // One coherent reader snapshot covers connection, active profile and
        // VoWiFi state, so the menu bar can never render a mixed generation.
        store.readerRuntime.$state
            .receive(on: RunLoop.main)
            .sink { [weak self, weak store] _ in
                guard let self else { return }
                self.renderMenuBar(
                    status: store?.status,
                    traffic: store?.traffic,
                    isStale: store?.statusStale ?? false)
            }
            .store(in: &statusCancellables)

        NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didWakeNotification)
            .receive(on: RunLoop.main)
            .sink { [weak store] _ in
                Task { @MainActor [weak store] in
                    store?.recoverNetworkAfterWake()
                }
            }
            .store(in: &statusCancellables)

        NotificationCenter.default.publisher(
            for: MenuBarDisplayOptions.didChangeNotification)
            .receive(on: RunLoop.main)
            .sink { [weak self, weak store] _ in
                guard let self else { return }
                self.renderMenuBar(
                    status: store?.status,
                    traffic: store?.traffic,
                    isStale: store?.statusStale ?? false)
            }
            .store(in: &statusCancellables)
    }

    private func consumeTrafficSample(_ snapshot: TrafficSnapshot?) {
        guard let snapshot,
              snapshot.available,
              let rxBytes = snapshot.rxBytes,
              let txBytes = snapshot.txBytes else {
            resetTrafficRate()
            return
        }

        let sampledAt = snapshot.sampledAtMS.map { TimeInterval($0) / 1_000 }
            ?? Date().timeIntervalSince1970
        let current = TrafficCounterSample(
            interfaceName: snapshot.interface,
            rxBytes: rxBytes,
            txBytes: txBytes,
            sampledAt: sampledAt)

        guard let previous = previousTrafficSample else {
            previousTrafficSample = current
            downloadBytesPerSecond = nil
            uploadBytesPerSecond = nil
            return
        }

        // 同一份快照可能因其他状态字段更新而被再次渲染，不应抹掉已算出的速率。
        guard current.sampledAt != previous.sampledAt else { return }

        let elapsed = current.sampledAt - previous.sampledAt
        guard current.interfaceName == previous.interfaceName,
              elapsed > 0,
              current.rxBytes >= previous.rxBytes,
              current.txBytes >= previous.txBytes else {
            previousTrafficSample = current
            downloadBytesPerSecond = nil
            uploadBytesPerSecond = nil
            return
        }

        downloadBytesPerSecond = Double(current.rxBytes - previous.rxBytes) / elapsed
        uploadBytesPerSecond = Double(current.txBytes - previous.txBytes) / elapsed
        previousTrafficSample = current
    }

    private func resetTrafficRate() {
        previousTrafficSample = nil
        downloadBytesPerSecond = nil
        uploadBytesPerSecond = nil
    }

    private func readerOperatorLabel(includeSource: Bool = true) -> String? {
        guard dashboardStore?.readerConnected == true,
              let usim = dashboardStore?.readerUSIMInfo else { return nil }
        let flag = iccidFlagEmoji(usim.iccid) ?? mccFlagEmoji(usim.mcc) ?? ""
        let label = "\(flag)\(usim.operatorDisplayName)"
        return includeSource ? "\(label)（读卡器）" : label
    }

    private func renderMenuBar(
        status: DeviceStatus?, traffic: TrafficSnapshot?, isStale: Bool
    ) {
        let displayOptions = MenuBarDisplayOptions()
        let readerSummary = readerOperatorLabel(includeSource: true)
            ?? (dashboardStore?.readerConnected == true ? "读卡器已连接" : nil)

        if isStale {
            let summary = ["模块数据过期", readerSummary].compactMap { $0 }.joined(separator: " · ")
            menuBarPresentation = MenuBarPresentation(
                image: menuBarImage(
                    options: displayOptions,
                    unavailableDescription: "模块信号暂时不可用"),
                rateTitles: displayOptions.rateTitles(download: "—", upload: "—"),
                downloadRate: "—",
                uploadRate: "—",
                networkSummary: summary,
                trafficSummary: "模块流量等待下一次刷新…")
            return
        }

        let moduleHasControlResponse = MenuBarStatusPolicy.moduleHasControlResponse(
            imei: status?.imei, iccid: status?.iccid, imsi: status?.imsi,
            operatorName: status?.operatorName, networkMode: status?.networkMode,
            simInserted: status?.simInserted, signalDbm: status?.signalDbm,
            registrationStatus: status?.regStatus)
        if status?.hardwareStatus?.isEmpty == false || !moduleHasControlResponse {
            resetTrafficRate()
            // 模块未连接但读卡器已连接时，状态栏显示读卡器 SIM 信息
            let readerConnected = dashboardStore?.readerConnected ?? false
            let readerOperator = readerOperatorLabel(includeSource: true)
            let networkSummary: String
            let trafficSummary: String
            let unavailableDesc: String
            if readerConnected, let op = readerOperator {
                networkSummary = op
                trafficSummary = "通过读卡器连接的 SIM 卡"
                unavailableDesc = op
            } else if readerConnected {
                networkSummary = "读卡器已连接"
                trafficSummary = "通过读卡器连接的 SIM 卡"
                unavailableDesc = "读卡器"
            } else {
                let deviceDetected = status?.usbDevice != nil
                let qmiInitialized = status?.qmi?.sessionInitialized == true
                networkSummary = qmiInitialized
                    ? "QMI 会话已初始化，模块状态待读取"
                    : (deviceDetected
                        ? "USB 模块已检测到，控制通道未就绪" : "未检测到模块")
                trafficSummary = qmiInitialized
                    ? "QMI 网络统计尚未读取，实时流量不可用"
                    : (deviceDetected
                        ? "模块控制接口尚未建立，实时流量不可用" : "实时流量不可用")
                unavailableDesc = networkSummary
            }
            menuBarPresentation = MenuBarPresentation(
                image: menuBarImage(
                    options: displayOptions,
                    unavailableDescription: unavailableDesc),
                rateTitles: displayOptions.rateTitles(download: "—", upload: "—"),
                downloadRate: "—",
                uploadRate: "—",
                networkSummary: networkSummary,
                trafficSummary: trafficSummary)
            return
        }

        let image = menuBarImage(
            options: displayOptions,
            signalDBM: status?.signalDbm,
            unavailableDescription: status == nil ? "正在读取网络信号" : "蜂窝信号暂时不可用")

        let operatorName = status?.operatorName?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let networkMode = status?.networkMode?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let networkParts = [operatorName, networkMode]
            .compactMap { value -> String? in
                guard let value, !value.isEmpty else { return nil }
                return value
            }
        let signalText = status?.signalDbm.map { "\($0) dBm" }
        let readerText = readerSummary
        let summaryParts = networkParts + [signalText, readerText].compactMap { $0 }

        let networkSummary: String
        if status?.simInserted == false {
            networkSummary = ["模块 SIM 未插入", readerSummary].compactMap { $0 }.joined(separator: " · ")
        } else if summaryParts.isEmpty {
            networkSummary = status == nil ? "正在读取模块状态…" : "模块状态待确认"
        } else {
            networkSummary = summaryParts.joined(separator: " · ")
        }

        let rateTitles: MenuBarRateTitles
        let downloadRate: String
        let uploadRate: String
        let trafficSummary: String
        if traffic?.available == true {
            let download = formatRate(downloadBytesPerSecond)
            let upload = formatRate(uploadBytesPerSecond)
            downloadRate = download
            uploadRate = upload
            rateTitles = displayOptions.rateTitles(download: download, upload: upload)
            if downloadBytesPerSecond == nil || uploadBytesPerSecond == nil {
                trafficSummary = "正在计算实时流量…"
            } else {
                trafficSummary = "下载 \(download)  ·  上传 \(upload)"
            }
        } else {
            downloadRate = "—"
            uploadRate = "—"
            rateTitles = displayOptions.rateTitles(download: "—", upload: "—")
            trafficSummary = traffic == nil
                ? "实时流量等待采样…" : "实时流量不可用"
        }

        menuBarPresentation = MenuBarPresentation(
            image: image,
            rateTitles: rateTitles,
            downloadRate: downloadRate,
            uploadRate: uploadRate,
            networkSummary: networkSummary,
            trafficSummary: trafficSummary)
    }

    private func menuBarImage(
        options: MenuBarDisplayOptions,
        signalDBM: Int? = nil,
        unavailableDescription: String
    ) -> NSImage? {
        if options.showSignal {
            if let signalDBM {
                return signalImage(for: signalDBM)
            }
            return NSImage(
                systemSymbolName: "exclamationmark.triangle",
                accessibilityDescription: unavailableDescription)
        }

        guard !options.hasSelection else {
            return nil
        }
        return NSImage(
            systemSymbolName: "antenna.radiowaves.left.and.right",
            accessibilityDescription: "Switch")
    }

    private func signalImage(for dbm: Int) -> NSImage {
        let level = signalLevel(for: dbm)
        if let cached = signalImageCache[level] {
            return cached
        }

        let imageWidth = CGFloat(15)
        let barWidth = CGFloat(2.6)
        let barStep = CGFloat(3.7)
        let barsWidth = barStep * 3 + barWidth
        let leadingInset = (imageWidth - barsWidth) / 2
        let image = NSImage(size: NSSize(width: imageWidth, height: 14), flipped: false) { _ in
            for index in 0..<4 {
                let height = CGFloat(4 + index * 3)
                let rect = NSRect(
                    x: leadingInset + CGFloat(index) * barStep,
                    y: 1,
                    width: barWidth,
                    height: height)
                let path = NSBezierPath(
                    roundedRect: rect, xRadius: 0.8, yRadius: 0.8)
                NSColor.black.withAlphaComponent(index < level ? 1 : 0.24).setFill()
                path.fill()
            }
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "蜂窝信号，4 格中的 \(level) 格"
        signalImageCache[level] = image
        return image
    }

    private func signalLevel(for dbm: Int) -> Int {
        switch dbm {
        case ..<(-100): return 0
        case ..<(-90): return 1
        case ..<(-80): return 2
        case ..<(-65): return 3
        default: return 4
        }
    }

    private func formatRate(_ bytesPerSecond: Double?) -> String {
        guard let bytesPerSecond,
              bytesPerSecond.isFinite,
              bytesPerSecond >= 0 else { return "—" }

        let units = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"]
        var value = bytesPerSecond
        var unitIndex = 0
        while value >= 1_024, unitIndex < units.count - 1 {
            value /= 1_024
            unitIndex += 1
        }

        let number: String
        if unitIndex == 0 || value >= 100 {
            number = String(
                format: "%.0f", locale: Locale(identifier: "en_US_POSIX"), value)
        } else if value >= 10 {
            number = String(
                format: "%.1f", locale: Locale(identifier: "en_US_POSIX"), value)
        } else {
            number = String(
                format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), value)
        }
        return "\(number) \(units[unitIndex])"
    }

    func showMainWindow() {
        guard mainWindowCoordinator.show() else {
            MainWindowRequestCenter.shared.requestOpen()
            return
        }
    }

    // MARK: - 窗口与生命周期

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        showMainWindow()
        return false
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if backendTermination.completed { return .terminateNow }
        // Do not block the main actor: BLE close/status receipts are serviced
        // here while the owned Rust process drains its actual device owners.
        backendTermination.request(backend: BackendProcess.shared,
            stopBridge: { if !AcceptanceBootstrap.isAcceptance { ReaderBLEBridge.shared.stop() } },
            reply: { sender.reply(toApplicationShouldTerminate: true) })
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        guard !backendTermination.completed else { return }
        // An externally forced termination may bypass the normal deferred
        // path. Request shutdown, but never invent an exit/physical receipt or
        // close the bridge while the child is still known to be alive.
        BackendProcess.shared.beginApplicationTermination()
        NSLog("App termination preceded owned backend exit confirmation")
    }

    // MARK: - 通知

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        // App 在前台时也弹出横幅（macOS 默认前台通知静默进通知中心）
        completionHandler([.banner, .sound, .list])
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let userInfo = response.notification.request.content.userInfo
        // 点击短信通知：激活窗口并打开短信页（来电提示已改为应用内自定义卡片，不走系统通知）
        let sender = userInfo["sender"] as? String
        let profileID = userInfo["profile_id"] as? String
        let messageID = userInfo["message_id"] as? String
        let isMultiStandby = userInfo["source"] as? String == "multistandby"
        let lineID = userInfo["device_id"] as? String
        let subscriptionID = userInfo["subscription_id"] as? String
        Task { @MainActor in
            if isMultiStandby, let profileID, let messageID {
                MultiStandbyStore.shared.requestOpenMessage(profileID: profileID, messageID: messageID)
                MainWindowRequestCenter.shared.requestOpen(destination: .sms)
            } else {
                MainWindowRequestCenter.shared.requestOpen(destination: .sms)
                SMSStore.shared.requestOpenSMS(sender: sender, lineID: lineID, messageID: messageID, subscriptionID: subscriptionID)
            }
        }
        completionHandler()
    }
}

struct MenuBarPresentation {
    let image: NSImage?
    let rateTitles: MenuBarRateTitles
    let downloadRate: String
    let uploadRate: String
    let networkSummary: String
    let trafficSummary: String

    var accessibilitySummary: String {
        "\(networkSummary)，\(trafficSummary)"
    }

    static var loading: MenuBarPresentation {
        let displayOptions = MenuBarDisplayOptions()
        let image: NSImage?
        if displayOptions.showSignal {
            image = NSImage(
                systemSymbolName: "ellipsis",
                accessibilityDescription: "正在读取网络信号")
        } else if displayOptions.hasSelection {
            image = nil
        } else {
            image = NSImage(
                systemSymbolName: "antenna.radiowaves.left.and.right",
                accessibilityDescription: "Switch")
        }

        return MenuBarPresentation(
            image: image,
            rateTitles: displayOptions.rateTitles(download: "—", upload: "—"),
            downloadRate: "—",
            uploadRate: "—",
            networkSummary: "正在读取网络状态…",
            trafficSummary: "实时流量等待采样…")
    }
}

struct MenuBarDisplayOptions {
    static let showSignalKey = "menuBarShowSignal"
    static let showDownloadKey = "menuBarShowDownloadRate"
    static let showUploadKey = "menuBarShowUploadRate"
    static let didChangeNotification = Notification.Name("menuBarDisplayOptionsDidChange")

    let showSignal: Bool
    let showDownload: Bool
    let showUpload: Bool

    init(defaults: UserDefaults = .standard) {
        showSignal = defaults.bool(forKey: Self.showSignalKey)
        showDownload = defaults.bool(forKey: Self.showDownloadKey)
        showUpload = defaults.bool(forKey: Self.showUploadKey)
    }

    var hasSelection: Bool {
        showSignal || showDownload || showUpload
    }

    func rateTitles(download: String, upload: String) -> MenuBarRateTitles {
        MenuBarRateTitles(
            download: showDownload ? "↓\(download)" : nil,
            upload: showUpload ? "↑\(upload)" : nil)
    }
}

struct MenuBarRateTitles {
    let download: String?
    let upload: String?

    var combined: String? {
        switch (download, upload) {
        case let (download?, upload?):
            // U+2004 在 macOS caption2 字体下约 3.45pt，与图文间距保持一致。
            return "\(download)\u{2004}\(upload)"
        case let (download?, nil):
            return download
        case let (nil, upload?):
            return upload
        case (nil, nil):
            return nil
        }
    }
}

private struct TrafficCounterSample {
    let interfaceName: String?
    let rxBytes: UInt64
    let txBytes: UInt64
    let sampledAt: TimeInterval
}
