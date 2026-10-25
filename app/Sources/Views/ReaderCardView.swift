import SwiftUI

/// Reader identity and eSIM profile controls live independently from the
/// module dashboard. Keeping their local sheets and operation state here
/// prevents HomeView from becoming a second reader runtime owner.
struct ReaderCardView<Communication: View>: View {
    @Environment(\.workspaceViewport) private var viewport
    @ObservedObject var store: DashboardStore
    @Binding var readerEnabled: Bool
    let privacyMaskEnabled: Bool
    let reduceMotion: Bool
    let communication: Communication

    init(store: DashboardStore, readerEnabled: Binding<Bool>, privacyMaskEnabled: Bool,
         reduceMotion: Bool, @ViewBuilder communication: () -> Communication) {
        self.store = store
        self._readerEnabled = readerEnabled
        self.privacyMaskEnabled = privacyMaskEnabled
        self.reduceMotion = reduceMotion
        self.communication = communication()
    }

    @StateObject private var readerService = BluetoothReaderService()
    @StateObject private var readerESIMStore = ReaderESIMStore()
    @State private var readerESIMExpanded = true
    @State private var readerRenamingProfile: ESIMProfileContext?
    @State private var readerDeletingProfile: ESIMProfileContext?
    @State private var readerEnablingProfile: ESIMProfileContext?
    @State private var readerShowingDownload = false
    @State private var readerShowingNotifications = false
    @State private var readerShowingBluetooth = false
    @State private var readerLegacyRecoveryReview: ReaderESIMStore.LegacyRecoveryCandidate?
    @State private var readerLegacyRecoveryReviewed = false
    @State private var readerLegacyRecoveryIDEntry = ""
    @State private var readerSwitchNotice: String?
    @State private var showIMSIdentity = false
    @State private var profileListContentHeight: CGFloat = 0
    @State private var profileQuery = ""
    @State private var enabledProfilesOnly = false

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            connectionPanel
            if let recovery = readerESIMStore.legacyRecovery {
                legacyRecoveryPanel(recovery)
            } else if let notice = readerESIMStore.legacyRecoveryNotice {
                Label(notice, systemImage: "checkmark.shield")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(viewport.cardPadding)
                    .workspaceCard()
            }
            let layout = viewport.contentWidth >= 640
                ? AnyLayout(HStackLayout(alignment: .top, spacing: viewport.sectionSpacing))
                : AnyLayout(VStackLayout(alignment: .leading, spacing: viewport.sectionSpacing))
            layout {
                if readerEnabled && store.readerConnected {
                    VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                        if let info = store.readerUSIMInfo {
                            currentProfileSummary(info)
                        } else {
                            identityPendingPanel
                        }
                        if store.readerHasESIM {
                            readerESIMSection
                                .padding(viewport.cardPadding)
                                .workspaceCard()
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                }
                communication.frame(maxWidth: .infinity, alignment: .topLeading)
            }
        }
        .sheet(isPresented: $readerShowingBluetooth) {
            ESTKBluetoothSheet(
                selectedReaderName: { store.readerName },
                selectionBlockReason: { bluetoothReaderSwitchBlockReason },
                onManualConnectionReady: { reader in
                    readerEnabled = true
                    store.connectReader(reader)
                },
                onTransportChanged: {
                    store.refreshReaderList(autoConnect: false)
                })
                .frame(width: min(560, max(460, viewport.width - 48)), height: min(480, max(330, viewport.height - 44)))
        }
        .sheet(item: $readerLegacyRecoveryReview) { recovery in
            legacyRecoveryReviewSheet(recovery)
        }
        .onAppear {
            synchronizeReaderESIM()
        }
        .compatibleOnChange(of: readerESIMContext) { _ in
            synchronizeReaderESIM()
        }
        .task {
            await readerESIMStore.observeLegacyRecovery()
        }
        .compatibleOnChange(of: store.readerName) { _ in
            profileQuery = ""
            enabledProfilesOnly = false
        }
        .compatibleOnChange(of: privacyMaskEnabled) { _ in
            // A query must not reveal matches for identifiers after masking.
            profileQuery = ""
        }
    }

    private func legacyRecoveryPanel(_ recovery: ReaderESIMStore.LegacyRecoveryCandidate) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "exclamationmark.shield")
                .foregroundStyle(.orange)
                .font(.title3)
            VStack(alignment: .leading, spacing: 5) {
                Text("上次 eSIM 写入结果尚未确认")
                    .font(.callout.weight(.semibold))
                Text("旧版操作记录无法确认当时使用的是哪张卡。请先核对可能涉及的卡片 Profile，再决定是否解除操作阻塞。")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let error = readerESIMStore.legacyRecoveryError {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }
            Spacer(minLength: 8)
            Button("核对记录") {
                readerLegacyRecoveryReviewed = false
                readerLegacyRecoveryIDEntry = ""
                readerLegacyRecoveryReview = recovery
            }
            .buttonStyle(.proletarius)
            .controlSize(.small)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(viewport.cardPadding)
        .workspaceCard()
    }

    private func legacyRecoveryReviewSheet(_ recovery: ReaderESIMStore.LegacyRecoveryCandidate) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("核对旧版 eSIM 操作", systemImage: "exclamationmark.shield")
                .font(.headline)
                .foregroundStyle(.orange)
            Text("这份旧版日志没有可验证的卡片身份。上次下载、切卡或删除可能已在卡上完成，也可能没有完成。确认后只会解除这条旧日志的安全阻塞，不会重试操作或向卡片发送指令。")
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            Text("操作编号：\(recovery.id)")
                .font(.caption.monospaced())
                .textSelection(.enabled)
            Text("请核对可能涉及的 eSIM 卡片 Profile 列表，然后输入操作编号末尾 \(ESIMLegacyRecoveryConfirmation.displayedIDSuffix(recovery.id).count) 位。")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField("编号末尾字符", text: $readerLegacyRecoveryIDEntry)
                .textFieldStyle(.workspace)
            Toggle("我已核对卡片，理解上次操作结果仍未知", isOn: $readerLegacyRecoveryReviewed)
                .toggleStyle(.checkbox)
                .font(.caption)
            if readerESIMStore.legacyRecovery?.id != recovery.id ||
                readerESIMStore.legacyRecovery?.rawUpdatedAt != recovery.rawUpdatedAt {
                Text("操作状态已变化，请关闭窗口后重新核对最新记录。")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            if let error = readerESIMStore.legacyRecoveryError {
                Text(error).font(.caption).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button("刷新状态") {
                    Task { await readerESIMStore.refreshLegacyRecovery() }
                }
                .buttonStyle(.proletarius)
                .controlSize(.small)
                Spacer()
                Button("取消") {
                    readerLegacyRecoveryReview = nil
                }
                .buttonStyle(.proletarius)
                Button(readerESIMStore.isAcknowledgingLegacyRecovery ? "正在确认…" : "确认已人工核对") {
                    Task {
                        let confirmed = await readerESIMStore.acknowledgeLegacyRecovery(
                            recovery,
                            reviewedByUser: readerLegacyRecoveryReviewed,
                            enteredIDSuffix: readerLegacyRecoveryIDEntry)
                        if confirmed { readerLegacyRecoveryReview = nil }
                    }
                }
                .buttonStyle(.proletariusPrimary)
                .disabled(readerESIMStore.isAcknowledgingLegacyRecovery ||
                    !readerESIMStore.legacyRecoveryStatusFresh ||
                    readerESIMStore.legacyRecovery?.id != recovery.id ||
                    readerESIMStore.legacyRecovery?.rawUpdatedAt != recovery.rawUpdatedAt ||
                    !ESIMLegacyRecoveryConfirmation.canSubmit(
                        reviewed: readerLegacyRecoveryReviewed,
                        enteredIDSuffix: readerLegacyRecoveryIDEntry,
                        operationID: recovery.id))
            }
        }
        .padding(24)
        .frame(width: min(520, max(380, viewport.width - 48)))
    }

    private var connectionPanel: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "creditcard")
                    .font(.system(size: 18, weight: .medium))
                    .foregroundStyle(WorkspaceColors.accent)
                VStack(alignment: .leading, spacing: 3) {
                    Text(store.readerConnected ? privateValue(readerService.displayName(forReader: store.readerName ?? "PC/SC 读卡器")) : "设备连接")
                        .font(.callout.weight(.semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help(store.readerConnected ? privateValue(readerService.displayName(forReader: store.readerName ?? "PC/SC 读卡器")) : "连接 USB 或蓝牙读卡器")
                        .copyableInformation(readerService.displayName(forReader: store.readerName ?? ""), enabled: store.readerConnected && !privacyMaskEnabled)
                    Text(store.readerConnected ? (store.readerName?.hasPrefix("BLE:") == true ? "蓝牙 · 已连接" : "USB · 已连接") : "USB / 蓝牙读卡器")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if store.readerConnected {
                    Button {
                        store.refreshSIM()
                        if store.readerHasESIM { readerESIMStore.refresh() }
                    } label: {
                        Image(systemName: "arrow.clockwise").frame(width: 28, height: 28)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(WorkspaceColors.accent)
                    .help("刷新卡片与 Profile 信息")
                    .accessibilityLabel("刷新卡片与 Profile 信息")
                    .disabled(store.readerActionInFlight || readerESIMStore.isBusy)
                    Button("断开") { store.disconnectReader() }
                        .buttonStyle(.proletarius)
                        .controlSize(.small)
                        .disabled(store.readerActionInFlight)
                }
                Button {
                    store.refreshReaderList()
                } label: {
                    Label("扫描", systemImage: "magnifyingglass")
                }
                .buttonStyle(.proletarius)
                .controlSize(.small)
                .disabled(store.readerActionInFlight)
                .help("扫描可用读卡器")
                Button {
                    readerShowingBluetooth = true
                } label: {
                    Image("BluetoothLogo")
                        .resizable()
                        .scaledToFit()
                        .frame(width: 18, height: 18)
                        .frame(width: 30, height: 30)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("扫描 ESTKme-RED 蓝牙读卡器")
                .accessibilityLabel("扫描蓝牙读卡器")
                Toggle("", isOn: Binding(
                    get: { readerEnabled },
                    set: { newValue in
                        readerEnabled = newValue
                        if !newValue {
                            if store.readerConnected {
                                store.disconnectReader()
                            } else {
                                store.clearReaderRuntimeState(clearList: true)
                            }
                        } else {
                            store.refreshReaderList()
                        }
                    }
                ))
                .toggleStyle(.switch)
                .controlSize(.small)
                .labelsHidden()
                .accessibilityLabel("读卡器功能")
            }

            if readerEnabled {
                if store.readerActionInFlight {
                    ProgressView().controlSize(.small)
                }
                if store.readerConnected {
                    if !otherReaders.isEmpty { readerSwitchSection }
                } else if store.readerListLoaded && !store.readerList.isEmpty {
                    readerPickerSection
                } else if store.readerListLoaded && store.readerList.isEmpty {
                    VStack(spacing: viewport.isCompactHeight ? 8 : 12) {
                        WorkspaceSymbol(systemName: "creditcard", tint: WorkspaceColors.accent, size: viewport.isCompactHeight ? 40 : 58)
                        Text("连接你的读卡器")
                            .font(.headline)
                        Text("插入 USB 读卡器后自动识别，或使用右上角蓝牙按钮扫描 ESTKme-RED。")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, viewport.isCompactHeight ? 12 : 24)
                } else {
                    ProgressView("正在扫描读卡器…")
                        .controlSize(.small)
                    .frame(maxWidth: .infinity)
                }

                if let err = store.readerError {
                    Label(err, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
                if let readerSwitchNotice {
                    Label(readerSwitchNotice, systemImage: "info.circle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }
        }
        .padding(.horizontal, viewport.cardPadding)
        .padding(.vertical, 10)
        .workspaceCard()
    }

    private var identityPendingPanel: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("正在确认当前号卡", systemImage: "simcard")
                .font(.headline)
            Text("设备仍已连接。确认身份后更新号码与运营商；未启用号卡时，可在下方下载或启用 Profile。")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text("号码 · 等待确认").font(.title3.monospacedDigit())
            Text("卡片标识与 IMS 参数待确认").font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: viewport.isCompactHeight ? 106 : 128, alignment: .leading)
        .padding(viewport.cardPadding)
        .workspaceCard()
    }

    private var readerESIMContext: ReaderESIMStore.ReaderContext {
        .init(name: store.readerName, connected: store.readerConnected, hasESIM: store.readerHasESIM)
    }

    private func synchronizeReaderESIM() {
        guard readerESIMStore.synchronizeReader(readerESIMContext) else { return }
        readerESIMExpanded = false
        profileListContentHeight = 0
        readerRenamingProfile = nil
        readerDeletingProfile = nil
        readerEnablingProfile = nil
        readerShowingDownload = false
        readerShowingNotifications = false
    }

    private var bluetoothReaderSwitchBlockReason: String? {
        if readerESIMStore.isBusy || readerESIMStore.loadPhase == .loading {
            return "eSIM 卡片正在读取或操作，请完成后再切换读卡器"
        }
        if store.readerActionInFlight { return "读卡器操作正在进行，请稍后重试" }
        if store.readerVoWiFiActionInFlight {
            return "Wi-Fi Calling 操作正在进行，请稍后切换读卡器"
        }
        guard store.readerConnected else { return nil }
        if store.readerName?.hasPrefix("BLE:") == true {
            // BluetoothReaderManager.connect() cancels the old GATT link
            // before the backend can reject a business-reader switch. Require
            // a deliberate backend disconnect first, even if status is stale.
            return "请先从读卡器卡片断开当前蓝牙读卡器，再更换设备"
        }
        guard store.readerVoWiFiLoaded, let status = store.readerVoWiFiStatus else {
            return "暂无法确认当前 Wi-Fi Calling 状态，请刷新后重试"
        }
        if status.tunnelState == "connecting" || status.tunnelState == "connected"
            || status.imsState == "registered" {
            return "请先关闭当前读卡器 Wi-Fi Calling，再切换蓝牙读卡器"
        }
        return nil
    }

    private var readerPickerSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            WorkspaceSectionHeader(title: "可用读卡器", detail: "选择设备以读取卡片和管理 Profile")
            ForEach(store.readerList, id: \.self) { name in
                Button {
                    if let reason = bluetoothReaderSwitchBlockReason {
                        readerSwitchNotice = reason
                        return
                    }
                    readerSwitchNotice = nil
                    store.connectReader(name)
                } label: {
                    HStack {
                        WorkspaceSymbol(systemName: "creditcard.fill", tint: WorkspaceColors.accent, size: 34)
                        Text(privateValue(readerService.displayName(forReader: name)))
                            .font(.callout)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                    .padding(12)
                    .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))
                }
                .buttonStyle(.plain)
                .disabled(store.readerActionInFlight)
            }
        }
    }

    /// Readers attached but not currently selected. Rendered below the
    /// connected-reader row so multiple USB readers stay visible.
    private var otherReaders: [String] {
        guard let current = store.readerName else { return store.readerList }
        return store.readerList.filter { $0 != current }
    }

    private var readerSwitchSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("其他设备")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            ForEach(otherReaders, id: \.self) { name in
                Button {
                    if let reason = bluetoothReaderSwitchBlockReason {
                        readerSwitchNotice = reason
                        return
                    }
                    readerSwitchNotice = nil
                    store.connectReader(name)
                } label: {
                    HStack {
                        Image(systemName: "creditcard")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(privateValue(readerService.displayName(forReader: name)))
                            .font(.callout)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        Text("连接")
                            .font(.caption)
                            .foregroundStyle(WorkspaceColors.accent)
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 10))
                }
                .buttonStyle(.plain)
                .disabled(store.readerActionInFlight)
            }
        }
    }

    // MARK: - 当前启用 Profile 概览

    /// 从 eSIM Profile 列表中找到当前启用的 Profile（state == 1）
    private var activeProfile: ProfileItem? {
        guard let identity = store.readerUSIMInfo?.iccid.filter(\.isNumber), !identity.isEmpty else { return nil }
        return readerESIMStore.profileGroups.flatMap { $0.profiles }
            .first { $0.profile.isEnabled && $0.profile.iccid.filter(\.isNumber) == identity }?.profile
    }

    private func currentProfileSummary(_ info: USIMInfo) -> some View {
        let profile = activeProfile
        let phone = usableSIMPhoneNumber(info.phoneNumber)
        let country = mccCountryDisplayName(info.mcc) ?? "未知地区"
        let flag = profile?.flagEmoji ?? mccFlagEmoji(info.mcc) ?? "🌐"
        let hasIMSIdentity = [info.privateIdentity, info.publicIdentity, info.homeDomain]
            .contains { !($0 ?? "").isEmpty }

        return VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text(flag)
                    .font(.system(size: 22))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text(profile?.displayName ?? info.operatorDisplayName)
                        .font(.system(size: 16, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .copyableInformation(profile?.displayName ?? info.operatorDisplayName)
                    HStack(spacing: 5) {
                        Text(store.readerHasESIM ? info.operatorDisplayName : "实体 SIM")
                            .copyableInformation(store.readerHasESIM ? info.operatorDisplayName : "实体 SIM")
                        Text("·")
                        Text(country)
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
                Spacer(minLength: 6)
                if profile?.isEnabled == true || !store.readerHasESIM {
                    Label("就绪", systemImage: "checkmark.circle.fill")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.green)
                } else if readerESIMStore.overview == nil {
                    ProgressView().controlSize(.mini)
                        .accessibilityLabel("正在读取 Profile")
                } else {
                    Label("待确认", systemImage: "clock")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.orange)
                }
            }

            HStack(alignment: .firstTextBaseline, spacing: 16) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("号码")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text(privateValue(phone ?? "号码未知"))
                        .font(.system(size: 19, weight: .semibold, design: .rounded).monospacedDigit())
                        .lineLimit(1)
                        .minimumScaleFactor(0.75)
                        .copyableInformation(phone ?? "", enabled: phone != nil && !privacyMaskEnabled)
                }
                Spacer(minLength: 6)
                VStack(alignment: .trailing, spacing: 3) {
                    Text("网络")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text("\(info.mcc)/\(info.mnc)")
                        .font(.caption.monospacedDigit().weight(.medium))
                        .lineLimit(1)
                        .copyableInformation("\(info.mcc)/\(info.mnc)")
                }
            }

            if store.readerHasESIM && profile == nil {
                Text("正在核对当前卡片与 Profile 身份…")
                    .font(.caption)
                    .foregroundStyle(readerESIMStore.overview == nil ? Color.secondary : Color.orange)
            }

            DisclosureGroup("卡片标识与 IMS 参数", isExpanded: $showIMSIdentity) {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 140), spacing: 14, alignment: .leading)], alignment: .leading, spacing: 10) {
                    readerInfoRow("ICCID", info.iccid, sensitive: true)
                    readerInfoRow("IMSI", info.imsi, sensitive: true)
                    readerInfoRow("国家网络", "\(country) · \(info.mcc)/\(info.mnc)")
                    if let impi = info.privateIdentity, !impi.isEmpty {
                        readerInfoRow("IMPI", impi, sensitive: true)
                    }
                    if let impu = info.publicIdentity, !impu.isEmpty {
                        readerInfoRow("IMPU", impu, sensitive: true)
                    }
                    if let domain = info.homeDomain, !domain.isEmpty {
                        readerInfoRow("Domain", domain, sensitive: true)
                    }
                    if !hasIMSIdentity {
                        Text("此卡未提供 IMS 身份参数")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                }
                .padding(.top, 6)
            }
            .font(.caption.weight(.medium))
            .tint(WorkspaceColors.accent)
        }
        .frame(maxWidth: .infinity, minHeight: viewport.isCompactHeight ? 106 : 128, alignment: .leading)
        .padding(viewport.cardPadding)
        .workspaceCard()
    }

    // MARK: - 读卡器 eSIM Profile 管理

    private var readerESIMSection: some View {
        VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 8 : 12) {
            HStack(spacing: 10) {
                Label("卡片 Profile", systemImage: "simcard")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.primary)
                if readerESIMStore.profileCount > 0 {
                    Text("\(readerESIMStore.profileCount)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    readerShowingDownload = true
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "arrow.down.to.line")
                            .font(.system(size: 12, weight: .semibold))
                        Text("下载")
                            .font(.caption.weight(.semibold))
                    }
                    .frame(minWidth: 40)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.proletariusPrimary)
                .controlSize(.small)
                .disabled(readerESIMStore.isBusy || readerESIMStore.capabilities?.canDownload != true)
                .help("通过激活码或二维码下载新的 eSIM Profile")

                Button {
                    readerShowingNotifications = true
                    readerESIMStore.loadNotifications()
                } label: {
                    ZStack(alignment: .topTrailing) {
                        Image(systemName: "bell")
                            .font(.caption2)
                        if !readerESIMStore.notifications.isEmpty {
                            Circle()
                                .fill(Color.red)
                                .frame(width: 5, height: 5)
                                .offset(x: 2, y: -1)
                        }
                    }
                    .frame(width: 28, height: 28)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(WorkspaceColors.accent)
                .disabled(readerESIMStore.isBusy)
                .help("查看并补发运营商 Profile 通知")

                Button {
                    if reduceMotion {
                        readerESIMExpanded.toggle()
                    } else {
                        withAnimation(ProletariusMotion.standard) {
                            readerESIMExpanded.toggle()
                        }
                    }
                } label: {
                    Image(systemName: readerESIMExpanded ? "chevron.down" : "chevron.right")
                        .font(.caption2)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help(readerESIMExpanded ? "收起 Profile 列表" : "展开 Profile 列表")
            }

            profileTaskStatus

            if readerESIMExpanded {
                if readerESIMStore.profileCount > 4 || !profileQuery.isEmpty || enabledProfilesOnly {
                    HStack(spacing: 8) {
                        TextField("搜索名称或运营商", text: $profileQuery)
                            .textFieldStyle(.workspace)
                            .controlSize(.small)
                        Toggle("已启用", isOn: $enabledProfilesOnly)
                            .toggleStyle(.checkbox)
                            .controlSize(.small)
                            .fixedSize()
                        if !profileQuery.isEmpty || enabledProfilesOnly {
                            Button {
                                profileQuery = ""
                                enabledProfilesOnly = false
                            } label: { Image(systemName: "xmark.circle") }
                            .buttonStyle(.plain)
                            .help("清除筛选，显示全部 Profile")
                            .accessibilityLabel("清除 Profile 筛选")
                        }
                    }
                }
                if readerESIMStore.isInitialLoading {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.mini)
                        Text("正在读取读卡器 eSIM…")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } else if case .failed(let msg) = readerESIMStore.loadPhase, readerESIMStore.overview == nil {
                    Text("读取失败：\(msg)")
                        .font(.caption)
                        .foregroundStyle(.red)
                    Button("重试") { readerESIMStore.load() }
                        .font(.caption)
                        .buttonStyle(.proletarius)
                        .controlSize(.small)
                } else if readerESIMStore.isPhysicalSIM {
                    Text("读卡器中的卡片为实体 SIM，不支持 eSIM Profile 管理")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else if readerESIMStore.profileCount > 0 {
                    if visibleProfiles.isEmpty {
                        Text("没有匹配的 Profile，清除筛选可查看全部。")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    ScrollView {
                        VStack(spacing: 8) {
                            ForEach(visibleProfiles) { context in
                                readerProfileRow(context)
                            }
                        }
                        .background {
                            GeometryReader { geometry in
                                Color.clear.preference(key: ReaderProfileListHeightKey.self, value: geometry.size.height)
                            }
                        }
                    }
                    .scrollIndicators(.visible)
                    .frame(height: visibleProfiles.isEmpty ? 0 : min(profileListContentHeight > 0 ? profileListContentHeight : profileListMaximumHeight, profileListMaximumHeight))
                    .onPreferenceChange(ReaderProfileListHeightKey.self) { height in
                        guard height.isFinite, height > 0,
                              abs(profileListContentHeight - height) > 0.5 else { return }
                        profileListContentHeight = height
                    }
                } else {
                    Text("无已安装 Profile")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .sheet(item: $readerRenamingProfile) { context in
            ReaderRenameProfileView(context: context, store: readerESIMStore)
        }
        .sheet(isPresented: $readerShowingDownload) {
            ReaderDownloadESIMView(store: readerESIMStore)
        }
        .sheet(isPresented: $readerShowingNotifications) {
            ReaderESIMNotificationsView(store: readerESIMStore, privacyMaskEnabled: privacyMaskEnabled)
        }
        .confirmationDialog("启用这个 Profile？", isPresented: Binding(
            get: { readerEnablingProfile != nil },
            set: { if !$0 { readerEnablingProfile = nil } }
        ), titleVisibility: .visible) {
            Button("启用 \(readerEnablingProfile?.profile.displayName ?? "Profile")") {
                guard let context = readerEnablingProfile else { return }
                readerEnablingProfile = nil
                readerESIMStore.switchProfile(context)
            }
            Button("取消", role: .cancel) {}
        } message: {
            Text("Profile 切换在 eUICC 层面完成，无需重启模块。")
        }
        .confirmationDialog("删除 Profile？", isPresented: Binding(
            get: { readerDeletingProfile != nil },
            set: { if !$0 { readerDeletingProfile = nil } }
        ), titleVisibility: .visible) {
            Button("删除 \(readerDeletingProfile?.profile.displayName ?? "Profile")", role: .destructive) {
                guard let context = readerDeletingProfile else { return }
                readerDeletingProfile = nil
                readerESIMStore.deleteProfile(context)
            }
            Button("取消", role: .cancel) {}
        } message: {
            Text("删除操作通常不可撤销。写入过程中不要拔出读卡器。")
        }
    }

    private func readerProfileRow(_ context: ESIMProfileContext) -> some View {
        let profile = context.profile
        let confirmedActive = activeProfile?.iccid == profile.iccid
        let phone = readerESIMStore.savedPhoneNumber(for: profile.iccid) ??
            (confirmedActive ? usableSIMPhoneNumber(store.readerUSIMInfo?.phoneNumber) : nil)
        return HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    if let flag = profile.flagEmoji {
                        Text(flag)
                            .font(.system(size: 15))
                    }
                    Text(profile.displayName)
                        .font(.system(size: 14, weight: .semibold))
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                        .copyableInformation(profile.displayName)
                    if profile.isEnabled {
                        Label(confirmedActive ? "已启用" : "待确认", systemImage: confirmedActive ? "checkmark.circle.fill" : "clock")
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(confirmedActive ? Color.green : Color.secondary)
                            .fixedSize()
                    }
                }
                Text(privateValue(profile.iccid))
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .copyableInformation(profile.iccid, enabled: !privacyMaskEnabled)
                HStack(spacing: 5) {
                    Text(profile.flagEmoji ?? phone.flatMap(phoneFlagEmoji) ?? "🌐")
                    Text("号码")
                        .foregroundStyle(.secondary)
                    Text(phone.map { privateValue($0) } ?? "未记录")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(phone == nil ? Color.secondary : Color.primary)
                        .copyableInformation(phone ?? "", enabled: phone != nil && !privacyMaskEnabled)
                }
            }
            Spacer(minLength: 0)
            if !profile.isEnabled {
                Button {
                    readerEnablingProfile = context
                } label: {
                    ZStack {
                        Text("启用").opacity(readerESIMStore.isProfileActionRunning(context) ? 0 : 1)
                        if readerESIMStore.isProfileActionRunning(context) {
                            ProgressView().controlSize(.mini)
                        }
                    }
                }
                .buttonStyle(.proletarius)
                .controlSize(.small)
                .disabled(readerESIMStore.isBusy || readerESIMStore.capabilities?.canSwitch != true)
            }
            Menu {
                Button {
                    readerRenamingProfile = context
                } label: {
                    Label("重命名", systemImage: "pencil")
                }
                .disabled(readerESIMStore.isBusy || readerESIMStore.capabilities?.canRename != true)
                Button(role: .destructive) {
                    readerDeletingProfile = context
                } label: {
                    Label("删除 Profile", systemImage: "trash")
                }
                .disabled(readerESIMStore.isBusy || readerESIMStore.capabilities?.canDelete != true || profile.isEnabled)
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(width: 28, height: 28)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .help("Profile 操作")
        }
        .padding(viewport.isCompactHeight ? 10 : 14)
        .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))
    }

    private var visibleProfiles: [ESIMProfileContext] {
        let query = profileQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        return readerESIMStore.profileGroups.flatMap { $0.profiles }.filter { context in
            guard !enabledProfilesOnly || context.profile.isEnabled else { return false }
            guard !query.isEmpty else { return true }
            // Search only public names; masking must not leak hidden numbers
            // through whether a row matches the entered query.
            return [context.profile.name, context.profile.serviceProviderName]
                .compactMap { $0 }.contains { $0.localizedStandardContains(query) }
        }
    }

    private var profileTaskStatus: some View {
        let banner = readerESIMStore.banner
        let operation = readerESIMStore.operation
        let working = readerESIMStore.action != nil || operation?.blocksNewMutation == true || readerESIMStore.isRefreshing
        let message = working ? (operation?.blocksNewMutation == true ? operation?.message ?? "正在确认卡片操作终态…" : "正在确认卡片信息…")
            : banner?.message ?? "选择 Profile 进行管理"
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                ZStack {
                    if working {
                        ProgressView().controlSize(.mini)
                    } else {
                        Image(systemName: banner?.tone == .error ? "exclamationmark.circle" : "info.circle")
                            .foregroundStyle(banner?.tone == .error ? Color.red : Color.secondary)
                    }
                }.frame(width: 16, height: 16)
                Text(message)
                    .font(.caption)
                    .foregroundStyle(banner?.tone == .error && !working ? Color.red : Color.secondary)
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(message)
                if banner != nil || operation != nil {
                    Image(systemName: "doc.on.doc")
                        .foregroundStyle(WorkspaceColors.accent)
                        .frame(width: 28, height: 28)
                        .copyableInformation(diagnosticSummary)
                        .help("复制不含号码、卡号或凭据的诊断摘要")
                        .accessibilityLabel("复制脱敏诊断摘要")
                }
            }
            .frame(minHeight: 36)
            if let detail = banner?.detail, !detail.isEmpty, !working {
                Text(privacyMaskEnabled ? "详细错误已遮盖，关闭隐私遮盖后可查看。" : detail)
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .copyableInformation(detail, enabled: !privacyMaskEnabled)
            }
        }
    }

    private var diagnosticSummary: String {
        // Deliberately allowlist structural facts. Server messages and names
        // can contain subscriber IDs or activation tokens, so omit them.
        let action: String
        switch readerESIMStore.action {
        case .switching: action = "switch_profile"
        case .deleting: action = "delete_profile"
        case .renaming: action = "rename_profile"
        case .downloading: action = "download_profile"
        case nil: action = "idle"
        }
        return "module=reader; transport=\(store.readerName?.hasPrefix("BLE:") == true ? "BLE" : "USB"); action=\(action); connected=\(store.readerConnected); identityConfirmed=\(store.readerUSIMInfo != nil); busy=\(readerESIMStore.isBusy); time=\(Date().ISO8601Format())"
    }

    private func readerInfoRow(_ label: String, _ value: String, sensitive: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(sensitive ? privateValue(value) : value)
                .font(.caption.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
                .copyableInformation(value, enabled: !sensitive || !privacyMaskEnabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func privateValue(_ value: String) -> String {
        PrivacyMask.value(value, enabled: privacyMaskEnabled)
    }

    private var profileListMaximumHeight: CGFloat {
        // Let the common two-profile case show the second row's number without
        // a tiny nested scroll, while keeping large collections bounded.
        let shortListAllowance: CGFloat = visibleProfiles.count <= 2 ? 16 : 0
        return max(110, min(310, viewport.height * 0.34 + shortListAllowance))
    }
}

private struct ReaderProfileListHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0

    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

/// Only a GATT connection caused by this sheet's explicit Connect action may
/// become the backend's selected APDU reader. A remembered automatic BLE
/// reconnect is transport recovery, not permission to replace a USB card.
struct ReaderBLEManualSelection {
    enum Resolution: Equatable {
        case none
        case select(String)
        case blocked(String)
    }

    private(set) var pendingID: UUID?

    mutating func request(_ id: UUID) { pendingID = id }
    mutating func cancel() { pendingID = nil }

    mutating func useConnected(_ id: UUID, blockReason: String?) -> Resolution {
        pendingID = nil
        if let blockReason { return .blocked(blockReason) }
        return .select("BLE:\(id.uuidString)")
    }

    mutating func connected(_ id: UUID?, blockReason: String?) -> Resolution {
        guard let id else { return .none }
        let explicitlyRequested = pendingID == id
        pendingID = nil
        guard explicitlyRequested else { return .none }
        if let blockReason { return .blocked(blockReason) }
        return .select("BLE:\(id.uuidString)")
    }
}

private struct ESTKBluetoothSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.workspaceViewport) private var viewport
    @StateObject private var service = BluetoothReaderService()
    @State private var manualSelection = ReaderBLEManualSelection()
    @State private var selectionNotice: String?
    @State private var credentialEditorID: UUID?
    let selectedReaderName: () -> String?
    let selectionBlockReason: () -> String?
    let onManualConnectionReady: (String) -> Void
    let onTransportChanged: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 14) {
                Image("BluetoothLogo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 24, height: 30)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text("蓝牙读卡器").font(.system(size: viewport.isCompactHeight ? 19 : 22, weight: .bold))
                    Text("发现并连接附近的 ESTKme-RED")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    service.scan()
                } label: {
                    Label("重新扫描", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.plain)
                .foregroundStyle(WorkspaceColors.accent)
                Button("完成") { dismiss() }
                    .buttonStyle(.proletarius)
                    .keyboardShortcut(.cancelAction)
            }
            HStack(spacing: 8) {
                Image(systemName: service.connectedID == nil ? "dot.radiowaves.left.and.right" : "checkmark.circle.fill")
                    .foregroundStyle(service.connectedID == nil ? WorkspaceColors.accent : Color.green)
                Text(service.statusText)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let selectionNotice {
                Text(selectionNotice)
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            if service.devices.isEmpty {
                VStack(spacing: 14) {
                    if service.isSearching {
                        ProgressView()
                    } else {
                        Image(systemName: service.errorCode == nil ? "antenna.radiowaves.left.and.right.slash" : "exclamationmark.triangle")
                            .font(.system(size: 30)).foregroundStyle(.secondary)
                    }
                    Text(service.statusText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    if service.errorCode == .unauthorized {
                        Button("打开蓝牙权限设置") {
                            if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth") {
                                NSWorkspace.shared.open(url)
                            }
                        }.buttonStyle(.proletarius)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(service.devices.keys.sorted {
                    (service.devices[$0] ?? "") < (service.devices[$1] ?? "")
                }, id: \.self) { id in
                    HStack(spacing: 8) {
                        Button {
                            if service.connectedID == id {
                                if selectedReaderName() == "BLE:\(id.uuidString)" {
                                    if let reason = selectionBlockReason() {
                                        selectionNotice = reason
                                        return
                                    }
                                    manualSelection.cancel()
                                    selectionNotice = nil
                                    service.disconnect()
                                } else {
                                    // A remembered GATT link is not automatically
                                    // the selected APDU reader. "使用" is the user's
                                    // explicit request to switch backend sessions.
                                    switch manualSelection.useConnected(id, blockReason: selectionBlockReason()) {
                                    case .select(let reader):
                                        selectionNotice = nil
                                        onManualConnectionReady(reader)
                                    case .blocked(let reason):
                                        selectionNotice = reason
                                    case .none:
                                        break
                                    }
                                }
                            } else {
                                if let reason = selectionBlockReason() {
                                    selectionNotice = reason
                                    return
                                }
                                manualSelection.request(id)
                                selectionNotice = nil
                                service.connect(id)
                            }
                        } label: {
                            HStack(spacing: 12) {
                                Label {
                                    Text(service.displayName(forReader: "BLE:\(id.uuidString)"))
                                } icon: {
                                    Image("BluetoothLogo")
                                        .resizable()
                                        .scaledToFit()
                                        .frame(width: 19, height: 22)
                                }
                                .font(.callout.weight(.medium))
                                Spacer()
                                Text(connectionActionTitle(for: id))
                                    .font(.callout.weight(.semibold))
                                    .foregroundStyle(WorkspaceColors.accent)
                            }
                            .padding(.vertical, viewport.isCompactHeight ? 6 : 10)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        if service.supportsNineEsimCredentials(id) {
                            Button {
                                credentialEditorID = id
                            } label: {
                                Image(systemName: "key.horizontal")
                                    .font(.callout.weight(.semibold))
                                    .foregroundStyle(WorkspaceColors.accent)
                                    .frame(width: 28, height: 28)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .help("设置这张 9eSIM 卡的蓝牙认证密码")
                            .popover(isPresented: Binding(
                                get: { credentialEditorID == id },
                                set: { if !$0 { credentialEditorID = nil } }
                            ), arrowEdge: .trailing) {
                                NineEsimCredentialEditor(
                                    readerID: id,
                                    service: service,
                                    onClose: { credentialEditorID = nil }
                                )
                                .frame(width: 340)
                                .padding(16)
                            }
                        }
                    }
                    .listRowBackground(WorkspaceColors.surface)
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .workspaceCard()
            }
            Text("手动连接成功后将选中该蓝牙读卡器；自动重连不会切换当前读卡器。")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(viewport.pageInset)
        .environment(\.workspaceAccent, WorkspaceColors.accent)
        .environment(\.workspaceVisualStyle, .nekoko)
        .proletariusPageBackground()
        .onAppear { service.scan() }
        .onDisappear {
            manualSelection.cancel()
            service.stopScan()
        }
        .compatibleOnChange(of: service.errorCode) { code in
            if code != nil { manualSelection.cancel() }
        }
        .compatibleOnChange(of: service.connectedID) { id in
            switch manualSelection.connected(id, blockReason: selectionBlockReason()) {
            case .select(let reader):
                onManualConnectionReady(reader)
            case .blocked(let reason):
                selectionNotice = reason
                onTransportChanged()
            case .none:
                onTransportChanged()
            }
        }
    }

    private func connectionActionTitle(for id: UUID) -> String {
        guard service.connectedID == id else { return "连接" }
        return selectedReaderName() == "BLE:\(id.uuidString)" ? "断开" : "使用"
    }
}

private struct NineEsimCredentialEditor: View {
    let readerID: UUID
    let service: BluetoothReaderService
    let onClose: () -> Void
    @State private var password = ""
    @State private var hasSavedPassword = false
    @State private var errorMessage: String?
    @State private var isLoading = true

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("9eSIM 蓝牙认证")
                .font(.headline)
            Text("密码按设备单独保存在此 Mac 的钥匙串中，下次连接生效。未设置时使用设备默认密码。")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("蓝牙卡密码", text: $password)
                .textFieldStyle(.roundedBorder)
                .disabled(isLoading)
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                if hasSavedPassword {
                    Button("恢复默认") { save(nil) }
                        .disabled(isLoading)
                }
                Spacer()
                Button("取消", action: onClose)
                    .keyboardShortcut(.cancelAction)
                Button("保存") { save(password) }
                    .buttonStyle(.proletarius)
                    .keyboardShortcut(.defaultAction)
                    .disabled(isLoading || password.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .onAppear(perform: load)
        .onDisappear { password = "" }
    }

    private func load() {
        do {
            password = try service.nineEsimPassword(for: readerID) ?? ""
            hasSavedPassword = !password.isEmpty
            isLoading = false
        } catch {
            errorMessage = "无法读取钥匙串中的认证密码。请检查 macOS 钥匙串状态后重试。"
            isLoading = false
        }
    }

    private func save(_ value: String?) {
        do {
            try service.saveNineEsimPassword(value, for: readerID)
            password = ""
            onClose()
        } catch {
            errorMessage = "无法安全保存蓝牙认证密码。"
        }
    }
}
