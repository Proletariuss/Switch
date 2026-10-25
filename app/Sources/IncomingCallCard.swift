import AppKit
import Combine
import SwiftUI

/// 自定义来电通知卡片：无边框浮层面板，替代系统通知。
/// 来电时固定在主屏幕右上角（最上层、不可拖动），不自动消失；
/// 挂断、通话结束或点击卡片后收起。
@MainActor
final class IncomingCallCard: ObservableObject {
    static let shared = IncomingCallCard()

    private var panel: NSPanel?
    private var controller: NSHostingController<IncomingCallCardContent>?
    private var ringSound: NSSound?
    private var ringTask: Task<Void, Never>?
    private var store: DashboardStore?
    private var preview = false
    /// 预览模式 30 秒自动收起任务（手动关闭时需注销，避免残留任务提前收起新卡片）
    private var previewHideTask: Task<Void, Never>?

    /// 卡片上显示的号码
    @Published private(set) var number: String = "未知号码"
    @Published private(set) var answerAvailable = false
    @Published private(set) var rejectInFlight = false
    @Published private(set) var failureMessage: String?
    private var callSubscription: AnyCancellable?

    private init() {}

    /// 弹出来电卡片并开始响铃。
    /// - Parameter store: 真实来电时传入（挂断走 store）；preview 模拟时传 nil
    func show(store: DashboardStore?, preview: Bool = false) {
        self.store = store
        self.preview = preview
        failureMessage = nil
        rejectInFlight = false
        self.number = store?.callStatus.number ?? (preview ? "+8613800000000" : "未知号码")
        if preview {
            self.answerAvailable = true
        } else if store?.callEndpoint == "reader" || store?.callEndpoint == "module-vowifi" {
            self.answerAvailable = ReaderCallReadiness.canAnswer(store?.readerCallState)
        } else {
            self.answerAvailable = store?.callModeStatus?.isReady == true
        }
        callSubscription?.cancel()
        if let store, !preview {
            callSubscription = store.callRuntime.objectWillChange
                .receive(on: RunLoop.main)
                .sink { [weak self, weak store] _ in
                    guard let self, let store else { return }
                    self.synchronize(store: store)
                }
        }
        // 注销上一次的自动收起任务（手动关闭后再触发时不会提前收起）
        previewHideTask?.cancel()
        previewHideTask = nil

        if panel == nil {
            let content = IncomingCallCardContent(card: self)
            let controller = NSHostingController(rootView: content)
            let panel = NSPanel(contentViewController: controller)
            panel.styleMask = [.nonactivatingPanel, .borderless]
            panel.isFloatingPanel = true
            panel.level = .statusBar
            panel.hidesOnDeactivate = false
            panel.isOpaque = false
            panel.backgroundColor = .clear
            // 圆角阴影由卡片自身的 SwiftUI shadow 绘制，关闭面板矩形阴影
            panel.hasShadow = false
            panel.invalidateShadow()
            panel.isMovable = false
            panel.animationBehavior = .utilityWindow
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            self.panel = panel
            self.controller = controller
        }
        guard let panel else { return }
        // 固定在主屏幕右上角（菜单栏下方）
        if let screen = NSScreen.main {
            let origin = NSPoint(
                x: screen.visibleFrame.maxX - panel.frame.width - 4,
                y: screen.visibleFrame.maxY - panel.frame.height - 4)
            panel.setFrameOrigin(origin)
        }
        panel.orderFrontRegardless()
        startRing()

        if preview {
            // 模拟卡片 30 秒后自动收起
            previewHideTask = Task { [weak self] in
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                guard !Task.isCancelled else { return }
                self?.hide()
            }
        }
    }

    /// 收起卡片并停止响铃（不挂断）
    func hide() {
        stopRing()
        previewHideTask?.cancel()
        previewHideTask = nil
        panel?.orderOut(nil)
        callSubscription?.cancel()
        callSubscription = nil
    }

    /// 提交拒接；真实来电等待状态确认结束后收起，失败时保留重试入口。
    func reject() {
        if let store, !preview {
            store.rejectCall()
            synchronize(store: store)
        } else {
            hide()
        }
    }

    /// Update an existing card without restarting its ringtone or preview timer.
    private func synchronize(store: DashboardStore) {
        guard !preview, store.callStatus.isIncoming else { return }
        number = store.callStatus.number ?? "未知号码"
        rejectInFlight = store.callRuntime.rejectInFlight
        failureMessage = rejectInFlight ? nil : store.voiceError
        let ready = store.callEndpoint == "reader" || store.callEndpoint == "module-vowifi"
            ? ReaderCallReadiness.canAnswer(store.readerCallState)
            : store.callModeStatus?.isReady == true
        answerAvailable = ready && !store.callRuntime.commandInFlight
    }

    /// 接听后拉起通话页，让用户立即看到模块音频建立状态。
    func answer() {
        guard let store, !preview else {
            openCallPage()
            return
        }
        answerAvailable = false
        openCallPage()
        store.answerCall()
    }

    /// 点击来电信息区域：拉起唯一主窗口并直接进入通话页。
    func openCallPage() {
        MainWindowRequestCenter.shared.requestOpen(destination: .calls)
        hide()
    }

    // MARK: - 响铃

    /// 铃声播放间隔：完整播完一段后停顿再响，像真实来电
    private static let ringPause: TimeInterval = 2.5

    private func startRing() {
        stopRing()
        scheduleRing()
    }

    private func scheduleRing() {
        guard let sound = loadRingSound() else { return }
        ringSound = sound
        sound.play()
        let interval = sound.duration + Self.ringPause
        ringTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.scheduleRing()
        }
    }

    private func stopRing() {
        ringTask?.cancel()
        ringTask = nil
        ringSound?.stop()
        ringSound = nil
    }

    /// 播放用户选中的铃声；静音时不播放；缺失时回退到默认铃声，再缺失回退系统音效
    private func loadRingSound() -> NSSound? {
        let id = Ringtones.selectedID()
        guard id != Ringtones.silentID else { return nil }
        if let url = Ringtones.soundURL(for: id) {
            return NSSound(contentsOf: url, byReference: true)
        }
        if let url = Ringtones.soundURL(for: Ringtones.defaultID) {
            return NSSound(contentsOf: url, byReference: true)
        }
        return NSSound(named: "Glass")
    }
}

/// 卡片内容
struct IncomingCallCardContent: View {
    @ObservedObject var card: IncomingCallCard
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    var body: some View {
        VStack(spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    SwitchBrandLogo(placement: .notification)
                    Text("来电")
                        .font(.callout.bold())
                        .foregroundStyle(WorkspaceColors.accent)
                }
                Text(PrivacyMask.value(card.number, enabled: privacyMaskEnabled))
                    .font(.title2.bold())
                    .textSelection(.enabled)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .onTapGesture {
                card.openCallPage()
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isButton)
            .accessibilityHint("打开 Switch 通话界面")
            .accessibilityAction {
                card.openCallPage()
            }

            Divider()

            if let message = card.failureMessage {
                Text(message).font(.caption).foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: 8) {
                Button {
                    card.reject()
                } label: {
                    Label(card.rejectInFlight ? "拒接中…" : "拒绝", systemImage: "phone.down.fill")
                }
                .buttonStyle(RedActionButtonStyle())
                .disabled(card.rejectInFlight)

                Spacer()

                Button {
                    card.answer()
                } label: {
                    Label("接听", systemImage: "phone.fill")
                }
                .buttonStyle(.proletariusPrimary)
                .disabled(!card.answerAvailable)
                .help(card.answerAvailable ? "接听来电" : "当前来电暂不可接听")
            }
        }
        .padding(20)
        .frame(width: 300)
        .workspaceCard()
        .environment(\.workspaceVisualStyle, .nekoko)
        .shadow(color: .black.opacity(0.12), radius: 4, y: 1)
        // 透明外边距：给阴影留出扩散空间，避免被窗口边缘裁剪成直角
        .padding(8)
    }
}

/// 红色主按钮样式（面板内 .tint 可能不生效，直接指定红色背景）
struct RedActionButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.callout.weight(.medium))
            .padding(.horizontal, 12)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 9)
                    .fill(Color.red.opacity(configuration.isPressed ? 0.75 : 1.0))
            )
            .foregroundStyle(.white)
    }
}
