import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// 拨号与通话为主要操作区，设备准备和音频设置按可用空间自适应排列。
struct CallView: View {
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @EnvironmentObject private var store: DashboardStore
    @EnvironmentObject private var attentionStore: AttentionStore
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.workspaceViewport) private var viewport
    @StateObject private var ringtonePreview = RingtonePreview()

    @AppStorage("callModeDownloadSource") private var downloadSource = "relay"
    @State private var showUSBConfirmation = false
    @State private var showDownloadConfirmation = false
    @State private var hasPromptedForDownload = false
    @State private var showAllHistory = false
    @State private var showAllBackups = false
    @State private var backupToRestore: CallModeUSBBackupSummary?
    @State private var showRestoreConfirmation = false
    @State private var backupToDelete: CallModeUSBBackupSummary?
    @State private var showDeleteConfirmation = false
    @State private var layoutMode = CallLayoutMode.compact
    @State private var showInCallKeypad = false
    @State private var sentDTMFDigits = ""

    private var mode: CallModeStatus? { store.callModeStatus }
    private var usesVoWiFiCallEndpoint: Bool {
        store.callEndpoint == "reader" || store.callEndpoint == "module-vowifi"
    }

    private var selectedVoWiFiReady: Bool {
        store.callEndpoint == "module-vowifi"
            ? store.voWiFiStatus?.callReady == true
            : store.readerVoWiFiStatus?.callReady == true
    }

    private var callReady: Bool {
        switch store.callEndpoint {
        case "reader": return store.readerVoWiFiStatus?.callReady == true
        case "module-vowifi": return store.voWiFiStatus?.callReady == true
        default: return mode?.isReady == true
        }
    }

    private var answerReady: Bool {
        switch store.callEndpoint {
        case "reader": return ReaderCallReadiness.canAnswer(store.readerCallState)
        case "module-vowifi": return ReaderCallReadiness.canAnswer(store.readerCallState)
        default: return mode?.isReady == true
        }
    }
    var body: some View {
        GeometryReader { proxy in
            VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                pageHeader
                responsiveCallContent
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .padding(viewport.pageInset)
            .frame(maxWidth: 1380)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .preference(
                key: CallLayoutModePreferenceKey.self,
                value: CallLayoutMode.resolve(width: max(0, proxy.size.width - viewport.pageInset * 2)))
        }
        .buttonStyle(.proletarius)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: store.callStatus.state)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.18), value: showInCallKeypad)
        .onPreferenceChange(CallLayoutModePreferenceKey.self) { newMode in
            guard layoutMode != newMode else { return }
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) {
                layoutMode = newMode
            }
        }
        .compatibleOnChange(of: store.callStatus.state) { newState in
            if newState == "idle" || newState == "unknown" {
                showInCallKeypad = false
                sentDTMFDigits = ""
            }
        }
        .onAppear {
            attentionStore.setViewingCalls(true)
            store.loadCallHistory()
            store.pollCallModeStatus()
            store.loadCallModeBackups()
            if mode?.isReady != true,
               store.readerVoWiFiStatus?.callReady == true {
                store.callEndpoint = "reader"
            }
            store.pollCallStatus()
            promptForDownloadIfNeeded(mode?.state)
        }
        .onDisappear {
            attentionStore.setViewingCalls(false)
            ringtonePreview.stop()
        }
        .compatibleOnChange(of: mode?.state) { state in
            promptForDownloadIfNeeded(state)
        }
        .compatibleOnChange(of: store.callEndpoint) { _ in
            store.pollCallStatus()
        }
        .confirmationDialog(
            usbConfirmationTitle,
            isPresented: $showUSBConfirmation,
            titleVisibility: .visible
        ) {
            Button(usbConfirmationActionTitle) {
                store.enableCallMode(
                    confirmADBAuthorization: mode?.requiresADBAuthorizationConfirmation == true)
            }
            Button("取消", role: .cancel) {}
        } message: {
            Text(usbConfirmationMessage)
        }
        .confirmationDialog(
            "下载模块侧语音运行时？",
            isPresented: $showDownloadConfirmation,
            titleVisibility: .visible
        ) {
            Button("下载并自动准备") {
                store.downloadCallModeRuntime(source: downloadSource)
            }
            Button("取消", role: .cancel) {}
        } message: {
            Text(downloadConfirmationMessage)
        }
        .confirmationDialog(
            "还原模块原始配置？",
            isPresented: $showRestoreConfirmation,
            titleVisibility: .visible
        ) {
            if let backup = backupToRestore {
                Button("还原并重启模块", role: .destructive) {
                    store.restoreCallModeBackup(backup)
                    backupToRestore = nil
                }
            }
            Button("取消", role: .cancel) {
                backupToRestore = nil
            }
        } message: {
            Text(restoreConfirmationMessage)
        }
        .confirmationDialog(
            "删除配置备份？",
            isPresented: $showDeleteConfirmation,
            titleVisibility: .visible
        ) {
            if let backup = backupToDelete {
                Button("删除备份", role: .destructive) {
                    store.deleteCallModeBackup(backup)
                    backupToDelete = nil
                }
            }
            Button("取消", role: .cancel) {
                backupToDelete = nil
            }
        } message: {
            Text(deleteConfirmationMessage)
        }
        .sheet(isPresented: $showAllHistory) {
            CallHistoryView()
        }
    }

    @ViewBuilder
    private var pageHeader: some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 6 : 12) {
            WorkspaceHero(
                title: "通话",
                subtitle: "选择呼叫设备，保持每一次连接清晰可控。",
                symbol: "phone.fill",
                eyebrow: "CALLS")
            HStack(spacing: 12) {
                pageStatusLabel
                Spacer(minLength: 0)
            }
        }
    }

    private var pageStatusLabel: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) {
                pageModeLabel
                pageCodecBadge
            }
            .fixedSize(horizontal: true, vertical: false)
            VStack(alignment: .leading, spacing: 6) {
                pageModeLabel
                pageCodecBadge
            }
        }
    }

    private var pageModeLabel: some View {
        Label(pageStatusText, systemImage: modeBadgeIcon)
            .font(.callout)
            .foregroundStyle(modeBadgeColor)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityLabel("通话模式：\(pageStatusText)")
    }

    @ViewBuilder
    private var pageCodecBadge: some View {
        if usesVoWiFiCallEndpoint {
            ProletariusStatusBadge(
                title: "EVS 优先 · AMR-WB 兼容",
                color: ProletariusPalette.calls,
                systemImage: "waveform")
                .help("优先协商 EVS 宽带 13.2 kbit/s；对端未接受时使用 AMR-WB；不支持时明确报错，不使用 AMR-NB 或 PCMA。实际编码以通话详情为准。")
        }
    }

    @ViewBuilder
    private var responsiveCallContent: some View {
        switch layoutMode {
        case .wide, .compact:
            HStack(alignment: .top, spacing: viewport.sectionSpacing) {
                ScrollView {
                    callWorkspace
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(minWidth: layoutMode == .wide ? 500 : 310, maxWidth: .infinity, maxHeight: .infinity)

                ScrollView {
                    VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                        selectedOperationalInspector
                        ringtoneOptionCard
                        if layoutMode == .compact && store.callStatus.isIdle {
                            recentCalls
                                .padding(viewport.cardPadding)
                                .workspaceCard()
                        }
                        if store.callEndpoint == "module" {
                            backupOptionCard
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(width: layoutMode == .wide ? 310 : 260)
            }
        case .stacked:
            ScrollView {
                VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                    if preparationLeads {
                        selectedCompactOperationalInspector
                    }
                    callWorkspace
                    ringtoneOptionCard
                    if !preparationLeads {
                        selectedCompactOperationalInspector
                    }
                    if store.callEndpoint == "module" {
                        backupOptionCard
                    }
                }
                .frame(maxWidth: .infinity, alignment: .topLeading)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private var callWorkspace: some View {
        VStack(spacing: 0) {
            HStack(alignment: .center, spacing: 10) {
                WorkspaceSymbol(systemName: "phone.fill", tint: WorkspaceColors.accent, size: 32)
                WorkspaceSectionHeader(title: callPanelTitle, detail: "呼叫控制")
                Spacer()
                callStateBadge
            }
            .padding(viewport.cardPadding)

            Divider()

            callEndpointPicker
                .padding(viewport.cardPadding)

            Divider()

            if let callError, !callError.isEmpty {
                callErrorBanner(callError)
                    .padding(14)
                Divider()
            }

            Group {
                if store.callStatus.isIdle {
                    dialerWorkbench
                } else if store.callStatus.state == "unknown" {
                    unknownCallState
                } else {
                    liveCall
                }
            }
            .padding(viewport.cardPadding)
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .workspaceCard()
    }

    private var dialerWorkbench: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            numberEntry
            responsiveDialerContent
        }
    }

    @ViewBuilder
    private var responsiveDialerContent: some View {
        switch layoutMode {
        case .wide:
            HStack(alignment: .top, spacing: viewport.sectionSpacing) {
                keypad
                    .frame(width: viewport.isCompactHeight ? 190 : 210, alignment: .top)
                Divider()
                recentCalls
                    .frame(minWidth: 210, maxWidth: .infinity, alignment: .topLeading)
            }
        case .compact:
            keypad
                .frame(maxWidth: 340)
                .frame(maxWidth: .infinity)
        case .stacked:
            VStack(alignment: .leading, spacing: 14) {
                keypad
                    .frame(maxWidth: 360)
                    .frame(maxWidth: .infinity)
                Divider()
                recentCalls
            }
        }
    }

    private var numberEntry: some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 4 : 8) {
            Text("电话号码")
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
            TextField("输入电话号码", text: $store.dialNumber)
                .textFieldStyle(.workspace)
                .font(.title3.monospacedDigit())
                .frame(minWidth: 60)
                .layoutPriority(1)
                .onSubmit { dial() }
                .accessibilityLabel("电话号码")

            Button("+") {
                appendPlus()
            }
            .controlSize(.large)
            .disabled(store.dialNumber.contains("+"))
            .help("添加国际号码前缀")
            .accessibilityLabel("添加国际号码前缀")

            Button {
                if !store.dialNumber.isEmpty {
                    store.dialNumber.removeLast()
                }
            } label: {
                Image(systemName: "delete.left")
            }
            .controlSize(.large)
            .disabled(store.dialNumber.isEmpty)
            .help("删除最后一位")
            .accessibilityLabel("删除最后一位")
            }
        }
    }

    private var callEndpointPicker: some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 6 : 10) {
            Text("拨号设备")
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
            ViewThatFits(in: .horizontal) {
                Picker("拨号设备", selection: $store.callEndpoint) {
                    if store.voWiFiStatus?.vowifiActive == true {
                        Text("DJI · Wi-Fi Calling").tag("module-vowifi")
                    } else {
                        Text("DJI 模块").tag("module")
                    }
                    Text(readerCallDeviceName).tag("reader")
                }
                .pickerStyle(.segmented)
                .fixedSize(horizontal: true, vertical: false)
                VStack(spacing: 6) {
                    if store.voWiFiStatus?.vowifiActive == true {
                        callEndpointOption("module-vowifi", title: "DJI · Wi-Fi Calling")
                    } else {
                        callEndpointOption("module", title: "DJI 模块")
                    }
                    callEndpointOption("reader", title: readerCallDeviceName)
                }
            }
            .disabled(callSelectionLocked)
            .help(callSelectionLocked ? "通话期间不能切换设备" : "选择用于拨号的设备")
        }
    }

    private func callEndpointOption(_ endpoint: String, title: String) -> some View {
        Button {
            store.callEndpoint = endpoint
        } label: {
            HStack(spacing: 8) {
                Text(title)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
                Image(systemName: store.callEndpoint == endpoint ? "checkmark.circle.fill" : "circle")
                    .accessibilityHidden(true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.proletarius)
        .overlay(RoundedRectangle(cornerRadius: 11).strokeBorder(
            store.callEndpoint == endpoint ? WorkspaceColors.accent : Color.clear,
            lineWidth: 1.2))
        .accessibilityValue(store.callEndpoint == endpoint ? "已选择" : "")
    }

    private var callSelectionLocked: Bool {
        store.callRuntime.commandInFlight || ["incoming", "dialing", "alerting", "active"].contains(store.callStatus.state)
    }

    private var readerCallDeviceName: String {
        let number = store.readerVoWiFiStatus?.registeredMSISDN?.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let number, !number.isEmpty else { return "Reader" }
        return "\(PrivacyMask.value(number, enabled: privacyMaskEnabled))_Reader"
    }

    private var keypad: some View {
        CallDialPad(
            number: $store.dialNumber,
            canDial: canDial,
            callReady: callReady,
            dialHelp: dialHelp,
            onDial: dial)
    }

    private var recentCalls: some View {
        CallRecentCallsView(
            records: store.callHistory,
            privacyMaskEnabled: privacyMaskEnabled,
            canRedial: canRedial,
            redialHelp: redialHelp,
            onRedial: redial,
            onDelete: { store.deleteCallRecord($0.id) },
            onShowAll: { showAllHistory = true })
    }

    private var unknownCallState: some View {
        VStack(spacing: 11) {
            Image(systemName: "questionmark.circle")
                .font(.system(size: 34))
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
            Text("正在确认通话状态")
                .font(.headline)
            Text("状态未知时不会发起新的呼叫。请重新同步模块状态后再继续。")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("重新同步") {
                store.pollCallStatus()
            }
            .buttonStyle(.proletariusPrimary)
        }
        .frame(maxWidth: .infinity, minHeight: viewport.isCompactHeight ? 220 : 300)
    }

    private var liveCall: some View {
        VStack(spacing: viewport.sectionSpacing) {
            WorkspaceSymbol(
                systemName: store.callStatus.isIncoming ? "phone.arrow.down.left.fill" : "phone.fill",
                tint: callStateColor,
                size: viewport.isCompactHeight ? 56 : 76)

            VStack(spacing: 5) {
                Text(PrivacyMask.value(store.callStatus.number ?? "未知号码", enabled: privacyMaskEnabled))
                    .font(.title.bold().monospacedDigit())
					.copyableInformation(store.callStatus.number ?? "", enabled: !privacyMaskEnabled)
                Text(callStateText)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            if store.callStatus.isActive || (usesVoWiFiCallEndpoint && store.callStatus.state == "alerting") {
                Label(
                    store.audioRunning ? activeAudioConnectionText : "正在建立通话音频…",
                    systemImage: store.audioRunning ? "waveform.circle.fill" : "waveform.circle")
                    .font(.callout)
                    .foregroundStyle(store.audioRunning ? Color.green : Color.secondary)
            }

            liveCallActions

            if store.callStatus.isActive && showInCallKeypad {
                inCallKeypad
                    .frame(maxWidth: 300)
                    .transition(reduceMotion ? .identity : .opacity.combined(with: .move(edge: .bottom)))
            }
        }
        .frame(maxWidth: .infinity, minHeight: viewport.isCompactHeight ? 240 : 340)
    }

    @ViewBuilder
    private var liveCallActions: some View {
        Group {
            if store.callStatus.isIncoming {
                centeredCallActions {
                    HStack(spacing: 12) {
                        answerCallButton
                        hangupCallButton
                    }
                }
            } else {
                centeredCallActions {
                    HStack(spacing: 12) {
                        if store.callStatus.isActive {
                            inCallKeypadButton
                        }
                        hangupCallButton
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .center)
    }

    private func centeredCallActions<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        ZStack(alignment: .center) {
            content()
                .fixedSize(horizontal: true, vertical: false)
        }
        .frame(maxWidth: .infinity, minHeight: 44, maxHeight: 44)
    }

    private var answerCallButton: some View {
        Button {
            store.answerCall()
        } label: {
            HStack(spacing: 7) {
                if store.callAnswerInFlight {
                    ProgressView()
                        .controlSize(.small)
                } else {
                    Image(systemName: "phone.fill")
                }
                Text(store.callAnswerInFlight ? "正在接听" : "接听")
            }
            .frame(minWidth: 84)
        }
        .buttonStyle(.proletariusPrimary)
        .controlSize(.large)
        .disabled(!answerReady || store.callAnswerInFlight)
        .help(answerReady ? "接听来电" : "当前来电暂不可接听")
    }

    private var inCallKeypadButton: some View {
        Button {
            showInCallKeypad.toggle()
        } label: {
            Label(showInCallKeypad ? "收起按键" : "拨号按键", systemImage: "circle.grid.3x3.fill")
                .frame(minWidth: 84)
        }
        .buttonStyle(.proletarius)
        .controlSize(.large)
        .help("发送客服电话菜单按键")
    }

    private var hangupCallButton: some View {
        Button(role: .destructive) {
            if store.callStatus.isIncoming {
                store.rejectCall()
            } else {
                store.hangup()
            }
        } label: {
            Label("挂断", systemImage: "phone.down.fill")
                .frame(minWidth: 84)
        }
        .buttonStyle(.proletariusPrimary)
        .controlSize(.large)
    }

    private var inCallKeypad: some View {
        CallInCallKeypad(sentDigits: $sentDTMFDigits) {
            store.sendDTMF($0)
        }
    }

    private func callErrorBanner(_ error: String) -> some View {
        let disconnected = isModuleDisconnectedError(error)
        return HStack(alignment: .top, spacing: 9) {
            Image(systemName: disconnected ? "usb.slash" : "exclamationmark.triangle.fill")
                .foregroundStyle(disconnected ? .orange : .red)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(disconnected ? "模块未连接" : "通话操作未完成")
                    .font(.callout.weight(.medium))
                Text(disconnected ? moduleDisconnectedFriendlyMessage : error)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    private var operationalInspector: some View {
        VStack(spacing: 0) {
            inspectorHeader
                .padding(14)

            Divider()

            VStack(spacing: 0) {
                readinessRow(
                    title: "模块接口",
                    detail: interfaceDetail,
                    state: interfaceStepState)
                Divider().padding(.leading, 29)
                readinessRow(
                    title: "运行时下载",
                    detail: runtimeDetail,
                    state: runtimeStepState)
                Divider().padding(.leading, 29)
                readinessRow(
                    title: "模块部署",
                    detail: deploymentDetail,
                    state: deploymentStepState)
            }
            .padding(.horizontal, 14)

            if hasPreparationDetail {
                Divider()
                preparationDetail
                    .padding(14)
            }

        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .glassCard()
    }

    @ViewBuilder
    private var selectedOperationalInspector: some View {
        if usesVoWiFiCallEndpoint {
            readerOperationalInspector
        } else {
            operationalInspector
        }
    }

    @ViewBuilder
    private var selectedCompactOperationalInspector: some View {
        if usesVoWiFiCallEndpoint {
            readerOperationalInspector
        } else {
            compactOperationalInspector
        }
    }

    private var readerOperationalInspector: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 9) {
                Image(systemName: selectedVoWiFiReady ? "checkmark.circle.fill" : "phone.connection")
                    .font(.title3)
                    .foregroundStyle(selectedVoWiFiReady ? Color.green : Color.orange)
                    .frame(width: 22)
                    .accessibilityHidden(true)

                VStack(alignment: .leading, spacing: 3) {
                    Text(selectedVoWiFiReady ? selectedVoWiFiTitle + " 已就绪" : selectedVoWiFiTitle + " 尚未就绪")
                        .font(.headline)
                    Text(readerCallDetail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Divider()

            Label(
                selectedVoWiFiReady ? "IMS 已注册" : (selectedVoWiFiRegistered ? "IMS 已注册，通话通道恢复中" : "IMS 未注册"),
                systemImage: selectedVoWiFiReady ? "checkmark.seal.fill" : "exclamationmark.triangle.fill")
                .font(.callout)
                .foregroundStyle(selectedVoWiFiReady ? Color.green : Color.orange)

            if let call = store.readerCallState,
               ["connecting", "early", "active"].contains(call.state) {
                Divider()
                Label(
                    readerMediaDescription(call),
                    systemImage: readerMediaTrafficObserved(call) ? "waveform.circle.fill" : "waveform.circle")
                    .font(.callout)
                    .foregroundStyle(readerMediaTrafficObserved(call) ? Color.green : Color.orange)

                if store.audioRunning {
                    Label(store.readerAudioPhase, systemImage: "speaker.wave.2.fill")
                        .font(.callout)
                    Label("输入位置·\(store.readerAudioInputName)", systemImage: "mic.fill")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Label("输出位置·\(store.readerAudioOutputName)", systemImage: "hifispeaker.fill")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Label(store.readerAudioProcessingName, systemImage: "waveform.badge.shield")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .workspaceCard()
    }

    private var readerCallReady: Bool {
        store.readerVoWiFiStatus?.callReady == true
    }

    private var selectedVoWiFiTitle: String {
        store.callEndpoint == "module-vowifi" ? "DJI 模块 VoWiFi" : "读卡器 VoWiFi"
    }

    private var selectedVoWiFiRegistered: Bool {
        store.callEndpoint == "module-vowifi"
            ? store.voWiFiStatus?.imsRegistered == true
            : store.readerVoWiFiStatus?.imsState == "registered"
    }

    private var readerCallDetail: String {
        if store.callEndpoint == "module-vowifi" {
            return store.voWiFiStatus?.summary ?? "请先在模块页面开启 Wi-Fi Calling"
        }
        if let message = store.readerVoWiFiStatus?.message,
           !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return message
        }
        return readerCallReady
            ? "呼叫信令通过当前读卡器 IMS 安全通道发送"
            : "请先在首页建立 Wi-Fi Calling 并完成 IMS 注册"
    }

    private var compactOperationalInspector: some View {
        VStack(spacing: 0) {
            inspectorHeader
                .padding(12)

            Divider()

            VStack(spacing: 0) {
                compactReadinessRow(title: "模块接口", state: interfaceStepState)
                Divider().padding(.leading, 27)
                compactReadinessRow(title: "运行时下载", state: runtimeStepState)
                Divider().padding(.leading, 27)
                compactReadinessRow(title: "模块部署", state: deploymentStepState)
            }
            .padding(.horizontal, 12)

            if hasPreparationDetail {
                Divider()
                preparationDetail
                    .padding(12)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .glassCard()
    }

    private var ringtoneOptionCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            WorkspaceSectionHeader(title: "来电铃声", detail: "Mac 音频提示")
            ringtoneControls
                .frame(maxWidth: .infinity, alignment: .leading)

            Text("来电时由 Mac 播放；静音不会影响模块侧呼叫状态。")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .workspaceCard()
    }

    private var backupOptionCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Text("模块配置备份")
                    .font(.callout.bold())
                Spacer()
                if !store.callModeBackups.isEmpty {
                    Text("\(store.callModeBackups.count) 项")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                if store.callModeBackupImporting {
                    ProgressView()
                        .controlSize(.small)
                        .accessibilityLabel("正在导入配置备份")
                } else {
                    Button {
                        chooseImportFile()
                    } label: {
                        Label("导入…", systemImage: "tray.and.arrow.down")
                    }
                    .controlSize(.small)
                    .disabled(
                        store.callModeBackupExportingID != nil
                            || store.callModeBackupDeletingID != nil)
                }
                Button {
                    store.loadCallModeBackups()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .controlSize(.small)
                .disabled(
                    store.callModeBackupsLoading
                        || store.callModeBackupImporting
                        || store.callModeBackupDeletingID != nil)
                .help("刷新配置备份")
                .accessibilityLabel("刷新配置备份")
            }

            backupContent
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .glassCard()
    }

    private var inspectorHeader: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 9) {
                Image(systemName: modeBadgeIcon)
                    .font(.title3)
                    .foregroundStyle(modeBadgeColor)
                    .frame(width: 22)
                    .accessibilityHidden(true)

                VStack(alignment: .leading, spacing: 3) {
                    Text(mode?.summary ?? "正在检查通话模式")
                        .font(.headline)
                    if let detail = mode?.detail, !detail.isEmpty {
                        Text(detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }

            modeAction
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
    }

    @ViewBuilder
    private var modeAction: some View {
        if mode?.isBusy == true || store.callModeActionInFlight {
            HStack(spacing: 7) {
                ProgressView()
                    .controlSize(.small)
                Text("正在准备…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .accessibilityLabel(mode?.summary ?? "正在准备通话模式")
        } else {
            switch mode?.state {
            case "needs_adb_authorization", "needs_usb", "needs_voice":
                Button("开启通话模式") {
                    showUSBConfirmation = true
                }
                .buttonStyle(.proletariusPrimary)
            case "needs_interface_recovery":
                Button("重启并修复接口") {
                    showUSBConfirmation = true
                }
                .buttonStyle(.proletariusPrimary)
            case "needs_download":
                Button("下载运行时") {
                    showDownloadConfirmation = true
                }
                .buttonStyle(.proletariusPrimary)
            case "failed":
                Button("重新检查并准备") {
                    store.retryCallModePreparation()
                }
                .buttonStyle(.proletariusPrimary)
            case "disconnected", "unsupported":
                Button("重新检测") {
                    store.retryCallModePreparation()
                }
            case "ready":
                EmptyView()
            default:
                Button("检查") {
                    store.pollCallModeStatus()
                }
            }
        }
    }

    private func readinessRow(title: String, detail: String, state: ReadinessState) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Group {
                switch state {
                case .complete:
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                case .actionRequired:
                    Image(systemName: "arrow.right.circle.fill")
                        .foregroundStyle(Color.accentColor)
                case .current:
                    ProgressView()
                        .controlSize(.mini)
                case .failed:
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                case .pending:
                    Image(systemName: "circle")
                        .foregroundStyle(.tertiary)
                }
            }
            .frame(width: 20, height: 20)
            .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(title)
                        .font(.callout.weight(.medium))
                    Spacer()
                    Text(state.accessibilityText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.vertical, 10)
        .accessibilityElement(children: .combine)
    }

    private func compactReadinessRow(title: String, state: ReadinessState) -> some View {
        HStack(spacing: 7) {
            Group {
                switch state {
                case .complete:
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                case .actionRequired:
                    Image(systemName: "arrow.right.circle.fill")
                        .foregroundStyle(Color.accentColor)
                case .current:
                    ProgressView()
                        .controlSize(.mini)
                case .failed:
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                case .pending:
                    Image(systemName: "circle")
                        .foregroundStyle(.tertiary)
                }
            }
            .frame(width: 20, height: 20)
            .accessibilityHidden(true)

            Text(title)
                .font(.callout.weight(.medium))
            Spacer(minLength: 6)
            Text(state.accessibilityText)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 9)
        .accessibilityElement(children: .combine)
    }

    private var preparationDetail: some View {
        VStack(alignment: .leading, spacing: 10) {
            if mode?.state == "downloading", let mode {
                VStack(alignment: .leading, spacing: 5) {
                    ProgressView(value: mode.downloadProgress)
                    HStack {
                        Text(byteProgress(mode))
                        Spacer()
                        Text("校验后保存")
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }

            if mode?.state == "needs_download" {
                VStack(alignment: .leading, spacing: 6) {
                    Picker("下载渠道", selection: $downloadSource) {
                        ForEach(mode?.sources ?? []) { source in
                            Text(source.name).tag(source.id)
                        }
                    }
                    .pickerStyle(.menu)

                    if let selected = selectedDownloadSource {
                        Text(selected.detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }

            if let error = store.callModeError, !error.isEmpty {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: isModuleDisconnectedError(error) ? "usb.slash" : "exclamationmark.triangle.fill")
                        .foregroundStyle(isModuleDisconnectedError(error) ? .orange : .red)
                        .accessibilityHidden(true)
                    Text(isModuleDisconnectedError(error) ? moduleDisconnectedFriendlyMessage : error)
                        .font(.caption)
                        .textSelection(.enabled)
                }
                .accessibilityElement(children: .combine)
            }

            if let mode, !mode.runtimePath.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("运行时 \(mode.runtimeVersion)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                    HStack(spacing: 10) {
                        Button("Finder 中显示") {
                            revealPath(mode.runtimePath)
                        }
                        .buttonStyle(.link)
                        .controlSize(.small)
                        Button("固定版本") {
                            openURL(mode.upstream)
                        }
                        .buttonStyle(.link)
                        .controlSize(.small)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var ringtoneControls: some View {
        HStack(spacing: 8) {
            Picker("来电铃声", selection: Binding(
                get: { Ringtones.selectedID() },
                set: {
                    ringtonePreview.stop()
                    Ringtones.setSelected($0)
                }
            )) {
                ForEach(Ringtones.all) { ringtone in
                    Text(ringtone.displayName).tag(ringtone.id)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)

            Button {
                ringtonePreview.toggle()
            } label: {
                Image(systemName: ringtonePreview.isPlaying ? "stop.circle.fill" : "play.circle.fill")
            }
            .disabled(Ringtones.selectedID() == Ringtones.silentID)
            .help(ringtonePreview.isPlaying ? "停止试听" : "试听当前铃声")
            .accessibilityLabel(ringtonePreview.isPlaying ? "停止试听" : "试听当前铃声")
        }
        .fixedSize(horizontal: true, vertical: false)
    }

    private var backupContent: some View {
        VStack(alignment: .leading, spacing: 11) {
            Text("修改 USB 配置前自动保存；可导入已导出的 JSON，只允许还原到同一模块。")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let error = store.callModeBackupError, !error.isEmpty {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: isModuleDisconnectedError(error) ? "usb.slash" : "exclamationmark.triangle.fill")
                        .foregroundStyle(isModuleDisconnectedError(error) ? .orange : .red)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 6) {
                        Text(isModuleDisconnectedError(error) ? moduleDisconnectedFriendlyMessage : error)
                            .font(.caption)
                            .textSelection(.enabled)
                        Button("重试") {
                            store.loadCallModeBackups()
                        }
                        .controlSize(.small)
                    }
                }
            }

            if store.callModeBackupsLoading && store.callModeBackups.isEmpty {
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text("正在读取配置备份…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
            } else if store.callModeBackups.isEmpty && store.callModeBackupError == nil {
                HStack(alignment: .top, spacing: 9) {
                    Image(systemName: "externaldrive.badge.timemachine")
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("还没有配置备份")
                            .font(.callout.weight(.medium))
                        Text("首次开启通话模式时会自动创建，也可以从 JSON 文件导入。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
            } else {
                VStack(spacing: 0) {
                    ForEach(visibleBackups) { backup in
                        backupRow(backup)
                        if backup.id != visibleBackups.last?.id {
                            Divider()
                        }
                    }
                }

                if store.callModeBackups.count > 3 {
                    Button(showAllBackups ? "收起较早备份" : "显示其余 \(store.callModeBackups.count - 3) 个备份") {
                        showAllBackups.toggle()
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    private func backupRow(_ backup: CallModeUSBBackupSummary) -> some View {
        let state = backupAvailability(backup)

        return VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(backupReasonText(backup))
                        .font(.callout.weight(.medium))
                    Text(backup.savedAt.formatted(date: .numeric, time: .shortened))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                Label(state.text, systemImage: state.icon)
                    .font(.caption)
                    .foregroundStyle(state.color)
            }

            backupIdentityLine(backup)

            backupActions(backup)
                .frame(maxWidth: .infinity, alignment: .trailing)

            if let detail = backup.detail, !detail.isEmpty {
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(backup.valid ? Color.secondary : Color.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.vertical, 8)
        .help(backupTechnicalDetail(backup))
        .accessibilityElement(children: .contain)
    }

    private func backupIdentityLine(_ backup: CallModeUSBBackupSummary) -> some View {
        Text("IMEI 尾号 \(backupIMEISuffix(backup)) · ADB \(backup.adbEnabled ? "开启" : "关闭") · UAC \(backup.uacEnabled ? "开启" : "关闭")")
            .font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .truncationMode(.middle)
    }

    private func backupActions(_ backup: CallModeUSBBackupSummary) -> some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 75), spacing: 8)], alignment: .leading, spacing: 8) {
            if store.callModeBackupExportingID == backup.id {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("正在导出备份")
            } else {
                Button {
                    chooseExportLocation(for: backup)
                } label: {
                    Label("导出…", systemImage: "square.and.arrow.up")
                }
                .controlSize(.small)
                .disabled(
                    store.callModeBackupExportingID != nil
                        || store.callModeBackupImporting
                        || store.callModeBackupDeletingID != nil)
            }

            Button("还原…") {
                backupToRestore = backup
                showRestoreConfirmation = true
            }
            .controlSize(.small)
            .disabled(!canRestore(backup))
            .help(restoreHelp(backup))

            if store.callModeBackupDeletingID == backup.id {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("正在删除配置备份")
            } else {
                Button(role: .destructive) {
                    backupToDelete = backup
                    showDeleteConfirmation = true
                } label: {
                    Label("删除…", systemImage: "trash")
                }
                .controlSize(.small)
                .disabled(!canDeleteBackup)
                .help("从本机删除这份配置备份")
            }
        }
    }

    private var callStateBadge: some View {
        Text(callStateText)
            .font(.caption.bold())
            .foregroundStyle(callStateColor)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Capsule().fill(callStateColor.opacity(0.13)))
            .accessibilityLabel("通话状态：\(callStateText)")
    }

    private var callPanelTitle: String {
        store.callStatus.isIdle ? "拨打电话" : "当前通话"
    }

    private var callStateText: String {
        switch store.callStatus.state {
        case "active": return "通话中"
        case "incoming": return "来电"
        case "dialing": return "正在拨号"
        case "alerting": return "等待接听"
        case "unknown": return "状态未知"
        default: return "空闲"
        }
    }

    private var callStateColor: Color {
        switch store.callStatus.state {
        case "active": return .green
        case "incoming": return .red
        case "dialing", "alerting": return .orange
        case "unknown": return .orange
        default: return .secondary
        }
    }

    private var activeAudioConnectionText: String {
        usesVoWiFiCallEndpoint
            ? store.readerAudioPhase
            : "Mac 与 DJI 模块音频已连接"
    }

    private func readerMediaDescription(_ call: ReaderVoWiFiCallState) -> String {
        if call.state == "active", call.mediaReady {
            let sent = call.rtpPacketsSent ?? 0
            let received = call.rtpPacketsReceived ?? 0
            let decodeErrors = call.rtpDecodeErrors ?? 0
            let delivered = call.audioFramesToHost ?? 0
            if sent > 0, received > 0, delivered > 0 {
                let decodeNote = decodeErrors > 0 ? " · AMR 解码异常 \(decodeErrors) 次" : ""
                return "RTP 上下行均有流量 · 本机收到 \(delivered) 帧\(decodeNote) · \(readerCodecLabel(call.codec))"
            }
            if received > 0, decodeErrors > 0 {
                return "已收到远端 RTP 包，但 AMR 解码异常 \(decodeErrors) 次"
            }
            if received > 0, delivered == 0 {
                return "已收到 \(received) 个远端 RTP 包，但尚未送达本机音频桥"
            }
            if decodeErrors > 0 {
                return "已收到远端 RTP 包，但 AMR 解码失败 \(decodeErrors) 次"
            }
            if sent > 0, received == 0 {
                return "已发送 \(sent) 个本地 RTP 包，尚未收到远端语音包"
            }
            return "媒体参数已协商，尚未检测到 RTP 语音包 · \(readerCodecLabel(call.codec))"
        }
        if call.earlyMedia, call.mediaReady {
            return "运营商早期媒体已协商 · \(readerCodecLabel(call.codec))"
        }
        if call.state == "early" {
            return "运营商未提供可用媒体，使用本地回铃音"
        }
        return "等待运营商媒体协商"
    }

    private func readerMediaTrafficObserved(_ call: ReaderVoWiFiCallState) -> Bool {
        call.state == "active"
            && (call.rtpPacketsSent ?? 0) > 0
            && (call.rtpPacketsReceived ?? 0) > 0
            && (call.audioFramesToHost ?? 0) > 0
    }

    private func readerCodecLabel(_ codec: String?) -> String {
        switch codec?.uppercased() {
        case "EVS": return "EVS 宽带 13.2 kbit/s · 16 kHz"
        case "AMR-WB": return "AMR-WB 宽带 · 16 kHz"
        case "AMR": return "AMR-NB 12.2 kbit/s"
        case let value?: return value
        case nil: return "编码待确认"
        }
    }

    private var callError: String? {
        store.voiceError ?? store.audioError
    }

    private var pageStatusText: String {
        if usesVoWiFiCallEndpoint {
            return selectedVoWiFiReady ? selectedVoWiFiTitle + " 已就绪" : selectedVoWiFiTitle + " 尚未就绪"
        }
        return mode?.summary ?? "正在检查通话模式"
    }

    private var modeBadgeIcon: String {
        if usesVoWiFiCallEndpoint {
            return selectedVoWiFiReady ? "checkmark.circle.fill" : "phone.connection"
        }
        switch mode?.state {
        case "ready": return "checkmark.circle.fill"
        case "failed", "unsupported": return "exclamationmark.triangle.fill"
        case "disconnected": return "cable.connector.slash"
        case "checking", "authorizing_adb", "enabling_usb", "enabling_voice", "restarting", "verifying_usb", "downloading", "preparing", "restoring_usb", "restarting_restore":
            return "arrow.triangle.2.circlepath"
        default: return "circle.dashed"
        }
    }

    private var modeBadgeColor: Color {
        if usesVoWiFiCallEndpoint {
            return selectedVoWiFiReady ? .green : .orange
        }
        switch mode?.state {
        case "ready": return .green
        case "failed", "unsupported": return .red
        case "checking", "authorizing_adb", "enabling_usb", "enabling_voice", "restarting", "verifying_usb", "downloading", "preparing", "restoring_usb", "restarting_restore":
            return .orange
        default: return .secondary
        }
    }

    private var interfaceStepState: ReadinessState {
        if mode?.interfacesReady == true { return .complete }
        if mode?.state == "failed" { return .failed }
        if ["needs_adb_authorization", "needs_usb", "needs_voice", "needs_interface_recovery"].contains(mode?.state ?? "") { return .actionRequired }
        if ["authorizing_adb", "enabling_usb", "enabling_voice", "restarting", "verifying_usb", "restoring_usb", "restarting_restore"].contains(mode?.state ?? "") {
            return .current
        }
        return .pending
    }

    private var runtimeStepState: ReadinessState {
        if mode?.runtimeDownloaded == true { return .complete }
        if mode?.state == "failed" && (mode?.downloadedBytes ?? 0) > 0 { return .failed }
        if mode?.state == "needs_download" { return .actionRequired }
        if mode?.state == "downloading" { return .current }
        return .pending
    }

    private var deploymentStepState: ReadinessState {
        if mode?.isReady == true { return .complete }
        if mode?.state == "failed" && mode?.runtimeDownloaded == true { return .failed }
        if mode?.state == "preparing" { return .current }
        return .pending
    }

    private var interfaceDetail: String {
        switch mode?.state {
        case "authorizing_adb":
            return "正在本机计算并提交 ADB 授权"
        case "enabling_usb":
            return "正在保留原配置并开启缺少的接口"
        case "enabling_voice":
            return "正在启用并回读 IMS/VoLTE"
        case "restarting":
            return "配置已提交，模块正在重新枚举"
        case "verifying_usb":
            return "正在核对 IMEI、IMS/VoLTE 与 ADB root"
        case "needs_interface_recovery":
            return "ADB 已配置但未响应，需要受控重启恢复"
        default:
            break
        }
        if mode?.adbAuthorizationRequired == true {
            return "ADB 待持久授权 · UAC \(mode?.uacEnabled == true ? "已开启" : "未开启")"
        }
        return "ADB \(mode?.adbEnabled == true ? "已开启" : "未开启") · UAC \(mode?.uacEnabled == true ? "已开启" : "未开启") · IMS/VoLTE \(mode?.voiceConfigured == true ? "已就绪" : "未就绪")"
    }

    private var runtimeDetail: String {
        if mode?.runtimeDownloaded == true { return "固定版本已下载并通过校验" }
        if mode?.state == "downloading", let mode { return byteProgress(mode) }
        return "由用户选择渠道下载并校验"
    }

    private var deploymentDetail: String {
        switch mode?.state {
        case "preparing": return "正在部署驱动并执行模块自检"
        case "ready": return "驱动、校准与 D4/UAC 路由启停已验证"
        case "failed" where mode?.runtimeDownloaded == true: return "部署或自检未通过，可重试"
        default: return "下载完成后自动部署到模块"
        }
    }

    private var hasPreparationDetail: Bool {
        mode?.state == "downloading"
            || mode?.state == "needs_download"
            || !(store.callModeError ?? "").isEmpty
            || !(mode?.runtimePath ?? "").isEmpty
    }

    private var selectedDownloadSource: CallModeDownloadSource? {
        mode?.sources.first(where: { $0.id == downloadSource })
    }

    private var preparationLeads: Bool {
        if usesVoWiFiCallEndpoint { return false }
        return mode?.isReady != true
    }

    private var visibleBackups: [CallModeUSBBackupSummary] {
        showAllBackups ? store.callModeBackups : Array(store.callModeBackups.prefix(3))
    }

    private var canDial: Bool {
        callReady
            && store.callStatus.isIdle
            && !store.callRuntime.commandInFlight
            && !store.dialNumber.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var dialHelp: String {
        if store.callRuntime.dialInFlight { return "正在提交拨号请求" }
        if !callReady {
            if store.callEndpoint == "module-vowifi", store.voWiFiStatus?.vowifiActive == true {
                return "模块 VoWiFi 通话安全通道尚未就绪"
            }
            return "请先完成通话模式准备"
        }
        if !store.callStatus.isIdle { return "请先结束当前呼叫" }
        if store.dialNumber.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "请输入电话号码" }
        return "拨打当前号码"
    }

    private func dial() {
        guard canDial else { return }
        store.dial(store.dialNumber)
    }

    private func appendPlus() {
        guard !store.dialNumber.contains("+") else { return }
        store.dialNumber.insert("+", at: store.dialNumber.startIndex)
    }

    private func canRedial(_ record: CallRecord) -> Bool {
        guard callReady(for: record), !callSelectionLocked else { return false }
        return !(record.number?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
    }

    private func callReady(for record: CallRecord) -> Bool {
        switch record.device {
        case "reader": return store.readerVoWiFiStatus?.callReady == true
        case "module":
            if store.voWiFiStatus?.vowifiActive == true {
                return store.voWiFiStatus?.callReady == true
            }
            return mode?.isReady == true
        default: return mode?.isReady == true
        }
    }

    private func redial(_ record: CallRecord) {
        guard canRedial(record), let number = record.number else { return }
        let dev = record.device ?? "module"
        store.callEndpoint = dev
        store.dialNumber = number
        store.dial(number)
    }

    private func redialHelp(_ record: CallRecord) -> String {
        guard !(record.number?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true) else {
            return "这条记录没有可拨打的号码"
        }
        if !callReady(for: record) { return "这条记录对应的拨号设备尚未就绪" }
        if callSelectionLocked { return "请先结束当前呼叫" }
        return "直接回拨"
    }

    private func backupReasonText(_ backup: CallModeUSBBackupSummary) -> String {
        if backup.imported { return "导入的模块配置" }
        switch backup.reason {
        case "before_call_mode": return "启用通话模式前"
        case "before_restore": return "还原前自动保护"
        default: return backup.schemaVersion == 0 ? "旧版配置备份" : "模块配置备份"
        }
    }

    private func backupIMEISuffix(_ backup: CallModeUSBBackupSummary) -> String {
        guard let imei = backup.moduleIMEI, imei.count >= 4 else { return "未知" }
        return String(imei.suffix(4))
    }

    private func backupAvailability(_ backup: CallModeUSBBackupSummary) -> (text: String, icon: String, color: Color) {
        if !backup.valid {
            return ("备份无效", "exclamationmark.triangle.fill", .red)
        }
        if !backup.restorable {
            return ("仅可导出", "square.and.arrow.down", .orange)
        }
        if mode?.state == "disconnected" || store.statusStale {
            return ("模块未连接", "cable.connector.slash", .secondary)
        }
        guard let currentIMEI = store.status?.imei, !currentIMEI.isEmpty else {
            return ("等待模块身份", "ellipsis.circle", .secondary)
        }
        guard currentIMEI == backup.moduleIMEI else {
            return ("其他模块", "person.crop.circle.badge.exclamationmark", .orange)
        }
        return ("当前模块", "checkmark.circle.fill", .green)
    }

    private func canRestore(_ backup: CallModeUSBBackupSummary) -> Bool {
        backup.valid
            && backup.restorable
            && backup.moduleIMEI == store.status?.imei
            && mode?.state != "disconnected"
            && !store.statusStale
            && store.callStatus.isIdle
            && !store.callModeActionInFlight
    }

    private var canDeleteBackup: Bool {
        !store.callModeActionInFlight
            && !store.callModeBackupImporting
            && store.callModeBackupExportingID == nil
            && store.callModeBackupDeletingID == nil
    }

    private func restoreHelp(_ backup: CallModeUSBBackupSummary) -> String {
        if !backup.valid { return "备份内容无效，不能还原" }
        if !backup.restorable { return "旧版备份缺少模块身份，只能导出" }
        if mode?.state == "disconnected" || store.statusStale { return "请先连接模块并等待状态刷新" }
        guard let currentIMEI = store.status?.imei, !currentIMEI.isEmpty else { return "正在等待模块身份" }
        if currentIMEI != backup.moduleIMEI { return "该备份属于另一台模块" }
        if !store.callStatus.isIdle { return "通话或呼叫进行中不能还原" }
        if store.callModeActionInFlight { return "请等待当前模块操作完成" }
        return "还原该备份并重启模块"
    }

    private func backupTechnicalDetail(_ backup: CallModeUSBBackupSummary) -> String {
        let identity = [backup.vendorID, backup.productID].compactMap { $0 }.joined(separator: ":")
        let flags = backup.flags?.map(String.init).joined(separator: ",") ?? "未知"
        let firmware = backup.firmware.flatMap { $0.isEmpty ? nil : $0 } ?? "未知"
        let voice = backup.voiceIncluded
            ? "IMS/VoLTE：\(backup.imsConfigured ? "已启用" : "已记录原始状态")"
            : "IMS/VoLTE：旧版备份未包含"
        return "文件：\(backup.fileName)\nUSB：\(identity.isEmpty ? "未知" : identity)\n功能位：\(flags)\n\(voice)\n固件：\(firmware)"
    }

    private var restoreConfirmationMessage: String {
        guard let backup = backupToRestore else { return "" }
        let time = backup.savedAt.formatted(date: .numeric, time: .shortened)
        let voiceScope = backup.voiceIncluded ? "，并还原当时的 IMS/VoLTE 配置" : "；这是旧版备份，IMS/VoLTE 不会改变"
        return "将把当前连接的模块还原到 \(time) 保存的配置（ADB \(backup.adbEnabled ? "开启" : "关闭")，UAC \(backup.uacEnabled ? "开启" : "关闭")\(voiceScope)）。应用会先自动备份当前配置，再写入、完整回读并重启模块；网络、短信和 eSIM 操作会短暂中断。本地下载的语音运行时不会删除。"
    }

    private var deleteConfirmationMessage: String {
        guard let backup = backupToDelete else { return "" }
        let time = backup.savedAt.formatted(date: .numeric, time: .shortened)
        return "将从本机永久删除 \(time) 的配置备份。此操作不会修改当前模块，但删除后无法在 Switch 中导出或还原这份备份。"
    }

    private var usbConfirmationTitle: String {
        if mode?.state == "needs_interface_recovery" {
            return "重启并恢复模块 ADB？"
        }
        return mode?.requiresADBAuthorizationConfirmation == true
            ? "持久授权模块 ADB？"
            : "开启通话模式？"
    }

    private var usbConfirmationActionTitle: String {
        if mode?.state == "needs_interface_recovery" {
            return "备份、重启并验证"
        }
        return mode?.requiresADBAuthorizationConfirmation == true
            ? "授权、开启并重启模块"
            : "开启并重启模块"
    }

    private var usbConfirmationMessage: String {
        if mode?.state == "needs_interface_recovery" {
            return "ADB/UAC 配置位已经开启，但真实 ADB root 通道没有响应。应用会先备份当前 USB 与 IMS/VoLTE 配置，再重新提交仅在本机计算的 ADB 授权密码；密码不会上传或保存。随后不扩大任何功能位，受控重启模块并重新验证同一 IMEI、完整配置和 ADB root。网络、短信和 eSIM 操作会中断约 20–60 秒。"
        }
        if mode?.requiresADBAuthorizationConfirmation == true {
            return "应用会先备份当前 USB 与 IMS/VoLTE 配置，再在本机计算授权密码并持久授权模块 ADB；密码不会上传或保存。随后只开启缺少的 ADB/UAC 功能位、启用 IMS/VoLTE 并重启模块。配置备份可以关闭 ADB 接口，但不能保证撤销持久授权。重启期间 4G 网络、短信和 eSIM 操作会中断约 20–60 秒。"
        }
        return "应用会先备份当前 USB 与 IMS/VoLTE 配置，只开启缺少的 ADB/UAC 功能位并启用 IMS/VoLTE，然后重启模块。重启后会验证同一模块、完整配置和真实 ADB root。网络、短信和 eSIM 操作会短暂中断，通常需要 20–60 秒恢复。"
    }

    private func chooseImportFile() {
        let panel = NSOpenPanel()
        panel.title = "导入模块配置备份"
        panel.message = "选择由 Switch 导出的 JSON 备份。导入只保存到本机，不会立即修改模块。"
        panel.prompt = "导入"
        panel.allowedContentTypes = [.json]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        guard panel.runModal() == .OK, let source = panel.url else { return }
        store.importCallModeBackup(from: source)
    }

    private func chooseExportLocation(for backup: CallModeUSBBackupSummary) {
        let panel = NSSavePanel()
        panel.title = "导出模块配置备份"
        panel.message = "备份包含模块 IMEI、USB 与 IMS/VoLTE 配置，请妥善保存。"
        panel.prompt = "导出"
        panel.nameFieldStringValue = backup.fileName
        panel.allowedContentTypes = [.json]
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        guard panel.runModal() == .OK, let destination = panel.url else { return }
        store.exportCallModeBackup(backup, to: destination)
    }

    private var downloadConfirmationMessage: String {
        let sourceName = selectedDownloadSource?.name ?? "所选渠道"
        let path = mode?.runtimePath ?? "Application Support/Switch"
        return "将从\(sourceName)下载固定提交 \(moduleShortVersion)，保存到 \(path)。每个文件都会验证大小与 SHA-256；校验失败的内容不会落盘或部署。"
    }

    private var moduleShortVersion: String {
        guard let version = mode?.runtimeVersion, !version.isEmpty else { return "QDC507 3.18.44" }
        return version
    }

    private func byteProgress(_ mode: CallModeStatus) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return "\(formatter.string(fromByteCount: mode.downloadedBytes)) / \(formatter.string(fromByteCount: mode.totalBytes))"
    }

    private func promptForDownloadIfNeeded(_ state: String?) {
        guard state == "needs_download", !hasPromptedForDownload else { return }
        hasPromptedForDownload = true
        DispatchQueue.main.async {
            showDownloadConfirmation = true
        }
    }

    private func revealPath(_ path: String) {
        let url = URL(fileURLWithPath: path, isDirectory: true)
        if FileManager.default.fileExists(atPath: path) {
            NSWorkspace.shared.activateFileViewerSelecting([url])
        } else {
            NSWorkspace.shared.open(url.deletingLastPathComponent())
        }
    }

    private func openURL(_ value: String) {
        guard let url = URL(string: value) else { return }
        NSWorkspace.shared.open(url)
    }
}
