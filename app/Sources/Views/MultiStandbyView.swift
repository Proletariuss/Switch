import SwiftUI

struct MultiStandbyView: View {
    @StateObject private var readerService = BluetoothReaderService()
    @EnvironmentObject private var store: MultiStandbyStore
    @Environment(\.workspaceViewport) private var viewport
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @State private var currentPage = 0
    @State private var search = ""
    @State private var profileFilter: MultiStandbyProfileFilter = .all
    @State private var anomaliesFirst = false
    @State private var choosingProfiles = false
    @State private var chosenProfileIDs = Set<String>()
    @State private var showingReport = false
    @State private var reportHours = 12

    let onOpenSMS: () -> Void

    private var orderedProfiles: [MultiStandbyProfile] {
        MultiStandbyPresentation.profiles(store.profiles, query: search, filter: profileFilter,
            anomaliesFirst: anomaliesFirst, masked: privacyMaskEnabled)
    }

    var body: some View {
        GeometryReader { geometry in
            let layout = pageLayout(for: geometry.size)
            let visibleProfiles = pageProfiles(for: layout)

            VStack(alignment: .leading, spacing: layout.spacing) {
                header
                safetyNotice
                controls(compact: layout.compactControls)

                if let banner = statusBanner {
                    statusBannerView(banner)
                }

                profileSectionHeader(layout)

                if orderedProfiles.isEmpty {
                    ContentUnavailableView {
                        Label(store.refreshing ? "正在读取号码" : "暂无号码", systemImage: "simcard")
                    } description: {
                        Text(store.profiles.isEmpty ? (store.refreshing ? "正在同步 eSIM Profile 状态。" : "连接号卡后点击刷新。") : "没有符合搜索或筛选条件的线路。")
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    LazyVGrid(columns: layout.columns, alignment: .leading, spacing: layout.spacing) {
                        ForEach(visibleProfiles) { profile in
                            profileCard(profile, height: layout.cardHeight)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    Spacer(minLength: 0)
                }
            }
            .padding(.horizontal, layout.pageInset)
            .padding(.vertical, layout.verticalInset)
            .frame(maxWidth: 1240, maxHeight: .infinity, alignment: .topLeading)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .onAppear {
            store.viewingPage = true
            selectDefaultCardIfNeeded(from: store.cards)
            store.refresh()
        }
        .onDisappear { store.viewingPage = false }
        .onChange(of: store.selectedCardID) { _, _ in currentPage = 0; chosenProfileIDs.removeAll() }
        .onChange(of: store.cards) { _, cards in selectDefaultCardIfNeeded(from: cards) }
        .onChange(of: search) { _, _ in resetFilterSelection() }
        .onChange(of: profileFilter) { _, _ in resetFilterSelection() }
        .onChange(of: anomaliesFirst) { _, _ in currentPage = 0 }
        .onChange(of: orderedProfiles.map(\.profileID)) { _, ids in chosenProfileIDs.formIntersection(ids) }
        .sheet(isPresented: $showingReport) { runtimeReportView }

    }

    private var header: some View {
        HStack(spacing: 10) {
            NWaitHeaderMark()
            VStack(alignment: .leading, spacing: 2) {
                Text("N待").font(.system(size: 21, weight: .semibold))
            }
            Spacer(minLength: 8)
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text("\(store.onlineCount)")
                    .font(.system(size: 20, weight: .semibold, design: .rounded).monospacedDigit())
                    .foregroundStyle(WorkspaceColors.accent)
                Text("/ \(store.profiles.count) 在线")
                    .font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(WorkspaceColors.accent.opacity(0.08), in: Capsule())
        }
        .frame(height: 40)
    }

    private var safetyNotice: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 12, weight: .semibold)).foregroundStyle(.orange)
            Text("安全提醒：请备份 eSIM 码号与恢复文件；Profile 丢失或实体卡芯片损坏，可能导致号码无法恢复。")
                .font(.system(size: 10, weight: .medium)).foregroundStyle(.primary)
                .lineLimit(1).minimumScaleFactor(0.78)
                .help("请妥善保存运营商提供的 eSIM 激活码和恢复文件；在没有确认备份可用前，不要删除或重置 Profile。实体卡芯片损坏也可能导致号码无法恢复。N待可监测 VoWiFi 掉线和 IMS 注册租期，当前无法读取运营商套餐的实际到期日。")
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .frame(height: 30)
        .workspaceCard(tint: .orange)
    }

    @ViewBuilder
    private func controls(compact: Bool) -> some View {
        VStack(spacing: compact ? 7 : 0) {
            HStack(spacing: 8) {
                Picker("号卡", selection: Binding(get: { pickerCardID }, set: store.selectCard)) {
                    ForEach(store.cards) { card in
                        Text("\(readerService.displayName(forReader: card.reader)) · \(card.profileCount) 个号码").tag(card.cardID)
                    }
                }
                .frame(maxWidth: compact ? .infinity : 360)

                if compact {
                    Spacer(minLength: 0)
                    reportButton
                    smsButton
                    refreshButton
                } else {
                    Spacer(minLength: 4)
                    startAllButton
                    stopAllButton
                    reportButton
                    smsButton
                    refreshButton
                }
            }

            if compact {
                HStack(spacing: 8) {
                    startAllButton
                    stopAllButton
                    Spacer(minLength: 0)
                    Label("短信专用", systemImage: "message.badge.filled.fill")
                        .font(.system(size: 10, weight: .medium)).foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        }
        .controlSize(.small)
        .padding(.horizontal, 10)
        .padding(.vertical, compact ? 8 : 9)
        .workspaceCard()
    }

    private var startAllButton: some View {
        Button { Task { await store.control(start: true) } } label: {
            Label("启动全部", systemImage: "power")
        }
        .buttonStyle(.proletariusPrimary)
        .disabled(!store.isServiceRunning || !store.selectedCardAvailable || store.controlling)
        .help("启动所选号卡的全部 N待 线路")
    }

    private var stopAllButton: some View {
        Button { Task { await store.control(start: false) } } label: {
            Label("停止全部", systemImage: "stop.fill")
        }
        .buttonStyle(.proletarius)
        .disabled(!store.isServiceRunning || store.selectedCardID.isEmpty || store.controlling)
        .help("停止所选号卡的全部 N待 线路")
    }

    private var reportButton: some View {
        Button { showingReport = true; Task { await store.refreshRuntimeReport(hours: reportHours) } } label: {
            Image(systemName: "clock.arrow.circlepath")
        }
        .buttonStyle(.proletarius)
        .disabled(!store.isServiceRunning)
        .help("运行记录与过夜统计")
        .accessibilityLabel("运行记录与过夜统计")
    }

    private var smsButton: some View {
        Button {
            store.requestSMSInbox()
            onOpenSMS()
        } label: {
            Image(systemName: "bubble.left.and.bubble.right.fill")
        }
        .buttonStyle(.proletarius)
        .disabled(store.profiles.isEmpty)
        .help("打开短信中心")
        .accessibilityLabel("打开短信中心")
    }

    private var refreshButton: some View {
        Button { store.refresh() } label: {
            Image(systemName: "arrow.clockwise")
        }
        .buttonStyle(.proletarius)
        .disabled(!store.isServiceRunning || store.refreshing)
        .help("刷新号卡和线路状态")
        .accessibilityLabel("刷新")
    }

    private var pickerCardID: String {
        if !store.selectedCardID.isEmpty { return store.selectedCardID }
        if let profile = store.profiles.first(where: { $0.profileID == store.selectedProfileID }) {
            return profile.cardID
        }
        return ""
    }

    private func selectDefaultCardIfNeeded(from cards: [MultiStandbyCard]) {
        guard store.viewingPage,
              store.selectedCardID.isEmpty,
              store.selectedProfileID.isEmpty,
              let card = cards.first(where: \.available) ?? cards.first else { return }
        store.selectCard(card.cardID)
    }

    private func resetFilterSelection() {
        currentPage = 0
        chosenProfileIDs.formIntersection(orderedProfiles.map(\.profileID))
    }

    private func profileSectionHeader(_ layout: PageLayout) -> some View {
        VStack(spacing: 5) {
            HStack(spacing: 8) {
                Text("线路").font(.system(size: 15, weight: .semibold))
                Text("\(orderedProfiles.count) / \(store.profiles.count) 条")
                    .font(.system(size: 11)).foregroundStyle(.secondary)
                if recoveringCount > 0 && !layout.compactControls {
                    Label("待恢复 \(recoveringCount)", systemImage: "arrow.clockwise.circle.fill")
                        .font(.system(size: 10, weight: .medium)).foregroundStyle(.orange)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Picker("状态", selection: $profileFilter) {
                    ForEach(MultiStandbyProfileFilter.allCases) { filter in Text(filter.rawValue).tag(filter) }
                }.labelsHidden().frame(width: 86)
                Toggle("异常优先", isOn: $anomaliesFirst).toggleStyle(.checkbox).font(.caption)
                if layout.pageCount > 1 {
                    Text("\(layout.pageIndex + 1) / \(layout.pageCount)")
                        .font(.system(size: 11, weight: .medium, design: .rounded).monospacedDigit())
                        .foregroundStyle(.secondary)
                    Button { currentPage = max(0, layout.pageIndex - 1) } label: { Image(systemName: "chevron.left") }
                        .buttonStyle(.proletarius).disabled(layout.pageIndex == 0)
                        .help("上一页线路").accessibilityLabel("上一页线路")
                    Button { currentPage = min(layout.pageCount - 1, layout.pageIndex + 1) } label: { Image(systemName: "chevron.right") }
                        .buttonStyle(.proletarius).disabled(layout.pageIndex >= layout.pageCount - 1)
                        .help("下一页线路").accessibilityLabel("下一页线路")
                }
            }
            HStack(spacing: 7) {
                TextField("搜索备注、号码或 ICCID 尾号", text: $search)
                    .textFieldStyle(.roundedBorder).font(.caption)
                Button {
                    choosingProfiles.toggle()
                    if !choosingProfiles { chosenProfileIDs.removeAll() }
                } label: { Image(systemName: choosingProfiles ? "checkmark.circle.fill" : "checkmark.circle") }
                    .buttonStyle(.proletarius).help("选择线路批量操作").accessibilityLabel("选择线路批量操作")
                if choosingProfiles {
                    Text("\(chosenProfileIDs.count) 已选").font(.caption).foregroundStyle(.secondary)
                    Button("启动所选") { Task { await store.control(start: true, profileIDs: Array(chosenProfileIDs)) } }
                        .buttonStyle(.proletariusPrimary)
                        .disabled(chosenProfileIDs.isEmpty || store.controlling || !store.selectedCardAvailable)
                    Button("停止所选") { Task { await store.control(start: false, profileIDs: Array(chosenProfileIDs)) } }
                        .buttonStyle(.proletarius)
                        .disabled(chosenProfileIDs.isEmpty || store.controlling || !store.isServiceRunning)
                }
            }
        }
        .controlSize(.small)
        .frame(height: 64)
    }

    private var statusBanner: StatusBanner? {
        if !store.isServiceRunning {
            return StatusBanner(title: "本机服务未就绪", detail: "服务恢复后会自动更新线路状态。", symbol: "pause.circle", color: .orange)
        } else if let message = store.actionError ?? store.lastError {
            return StatusBanner(title: "状态同步需要关注", detail: message, symbol: "exclamationmark.triangle", color: .orange)
        } else if let operation = store.operation, !operation.isFinished {
            return StatusBanner(title: operation.detail ?? operation.state, detail: nil, symbol: "arrow.triangle.2.circlepath", color: WorkspaceColors.accent, showsProgress: true)
        } else if store.cards.isEmpty {
            return StatusBanner(title: "尚未发现可用号卡", detail: "连接读卡器后点击刷新。", symbol: "simcard", color: .secondary)
        }
        return nil
    }

    private func statusBannerView(_ banner: StatusBanner) -> some View {
        HStack(spacing: 8) {
            if banner.showsProgress { ProgressView().controlSize(.small) }
            Image(systemName: banner.symbol).foregroundStyle(banner.color)
            Text(banner.title).font(.system(size: 11, weight: .medium)).lineLimit(1)
            if let detail = banner.detail {
                Text(detail).font(.system(size: 10)).foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.tail).help(detail)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10).padding(.vertical, 7)
        .frame(height: 32)
        .workspaceCard(tint: banner.color)
    }

    private func profileCard(_ profile: MultiStandbyProfile, height: CGFloat) -> some View {
        let selected = store.selectedProfileID == profile.profileID
        let error = profile.lastError.flatMap { $0.isEmpty ? nil : $0 }
        return VStack(alignment: .leading, spacing: 5) {
            Button {
                if choosingProfiles {
                    if chosenProfileIDs.contains(profile.profileID) { chosenProfileIDs.remove(profile.profileID) }
                    else { chosenProfileIDs.insert(profile.profileID) }
                } else { store.selectProfile(profile.profileID) }
            } label: {
                HStack(spacing: 7) {
                    Image(systemName: choosingProfiles ? (chosenProfileIDs.contains(profile.profileID) ? "checkmark.circle.fill" : "circle") : "simcard.fill")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(selected ? WorkspaceColors.accent : Color.secondary)
                        .frame(width: 27, height: 27)
                        .background(WorkspaceColors.accent.opacity(selected ? 0.12 : 0.07), in: RoundedRectangle(cornerRadius: 8))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(profile.label(masked: privacyMaskEnabled))
                            .font(.system(size: 12, weight: .semibold))
                            .lineLimit(1).truncationMode(.middle)
                        Text(error ?? (profile.physicalEnabled == true ? "实体卡当前启用" : "eSIM Profile"))
                            .font(.system(size: 9)).foregroundStyle(error == nil ? Color.secondary : .orange)
                            .lineLimit(1).truncationMode(.tail).help(error ?? "")
                    }
                    Spacer(minLength: 3)
                    readinessBadge(profile)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("选择 \(profile.label(masked: privacyMaskEnabled))")

            HStack(spacing: 5) {
                statusChip("鉴权", profile.authState)
                statusChip("隧道", profile.tunnelState)
                statusChip("IMS", profile.imsState)
                Spacer(minLength: 0)
            }

            HStack(spacing: 6) {
                Button {
                    Task {
                        if profile.isStopped { await store.control(start: true, profileID: profile.profileID) }
                        else { await store.retry(profileID: profile.profileID) }
                    }
                } label: { Label(profile.isStopped ? "启动" : "重试", systemImage: "play.fill") }
                    .buttonStyle(.proletariusPrimary)
                    .disabled(!store.isServiceRunning || store.controlling || profile.isReady)
                Button {
                    Task { await store.control(start: false, profileID: profile.profileID) }
                } label: { Label("停止", systemImage: "stop.fill") }
                    .buttonStyle(.proletarius)
                    .disabled(!store.isServiceRunning || store.controlling || profile.isStopped)
                Spacer(minLength: 0)
            }
        }
        .controlSize(.small)
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(height: height, alignment: .center)
        .workspaceCard(tint: (choosingProfiles ? chosenProfileIDs.contains(profile.profileID) : selected) ? WorkspaceColors.accent : nil)
    }

    private func readinessBadge(_ profile: MultiStandbyProfile) -> some View {
        let plannedRecovery = isRegistrationRenewing(profile) && !profile.isReady && !profile.isStopped
        let color: Color = profile.isReady ? .green : (plannedRecovery ? WorkspaceColors.accent : (profile.lastError?.isEmpty == false ? .orange : .secondary))
        let title: String
        let symbol: String
        if profile.isReady {
            title = "在线"; symbol = "checkmark.circle.fill"
        } else if profile.isStopped {
            title = "已停止"; symbol = "circle"
        } else if plannedRecovery {
            title = profile.recoveryReason == "planned_ike_reauth" ? "重认证中" : "续期中"
            symbol = "arrow.clockwise.circle.fill"
        } else if profile.health == "offline" {
            title = "掉线"; symbol = "wifi.exclamationmark"
        } else if isRecovering(profile) {
            title = "恢复中"; symbol = "arrow.triangle.2.circlepath"
        } else {
            title = "连接中"; symbol = "arrow.triangle.2.circlepath"
        }
        return Label(title, systemImage: symbol)
            .font(.system(size: 9, weight: .medium))
            .foregroundStyle(color)
            .padding(.horizontal, 6).padding(.vertical, 4)
            .background(color.opacity(0.09), in: Capsule())
            .help(runtimeDetail(profile))
    }

    private func statusChip(_ title: String, _ state: String?) -> some View {
        let value = stateTitle(state)
        let color: Color = value == "就绪" ? .green : (value == "失败" ? .orange : .secondary)
        return HStack(spacing: 3) {
            Circle().fill(color).frame(width: 4, height: 4)
            Text(title)
            Text(value).foregroundStyle(color)
        }
        .font(.system(size: 9, weight: .medium))
        .padding(.horizontal, 5).padding(.vertical, 3)
        .background(Color.secondary.opacity(0.055), in: Capsule())
        .lineLimit(1).minimumScaleFactor(0.8)
    }

    private func pageLayout(for size: CGSize) -> PageLayout {
        let inset: CGFloat = size.height < 580 ? 12 : 16
        let gap: CGFloat = 9
        let contentWidth = max(0, size.width - inset * 2)
        let minimumCardWidth: CGFloat = 224
        let columnCount = max(1, min(5, Int((contentWidth + gap) / (minimumCardWidth + gap))))
        let compactControls = contentWidth < 760
        let hasBanner = statusBanner != nil
        let cardHeight: CGFloat = size.width < 560 ? 120 : 116
        let controlsHeight: CGFloat = compactControls ? 90 : 50
        let bannerHeight: CGFloat = hasBanner ? 38 : 0
        let verticalInset: CGFloat = size.height < 580 ? 8 : 10
        let interItemSpacing: CGFloat = 8
        let sectionHeight: CGFloat = 64
        let edgeInsetsHeight = verticalInset * 2
        let headerStackHeight: CGFloat = 40 + 30 + sectionHeight
        let optionalStatusHeight = bannerHeight
        let controlStackHeight = controlsHeight
        let gapCount: CGFloat = hasBanner ? 5 : 4
        let fixedHeight: CGFloat = edgeInsetsHeight + headerStackHeight + optionalStatusHeight
            + controlStackHeight + gapCount * interItemSpacing
        let availableGridHeight = max(cardHeight, size.height - fixedHeight)
        let rows = max(1, Int((availableGridHeight + gap) / (cardHeight + gap)))
        let pageSize = max(1, columnCount * rows)
        let pageCount = max(1, (orderedProfiles.count + pageSize - 1) / pageSize)
        let pageIndex = min(currentPage, pageCount - 1)
        return PageLayout(
            columns: Array(repeating: GridItem(.flexible(), spacing: gap, alignment: .top), count: columnCount),
            pageSize: pageSize,
            pageCount: pageCount,
            pageIndex: pageIndex,
            cardHeight: cardHeight,
            pageInset: inset,
            verticalInset: verticalInset,
            spacing: interItemSpacing,
            compactControls: compactControls
        )
    }

    private func pageProfiles(for layout: PageLayout) -> [MultiStandbyProfile] {
        let start = layout.pageIndex * layout.pageSize
        return Array(orderedProfiles.dropFirst(start).prefix(layout.pageSize))
    }

    private var recoveringCount: Int {
        orderedProfiles.filter { profile in
            !profile.isReady && !profile.isStopped
                && (profile.health == "offline" || isRecovering(profile) || isRegistrationRenewing(profile))
        }.count
    }

    private func isRegistrationRenewing(_ profile: MultiStandbyProfile) -> Bool {
        if ["planned_renewal", "planned_ike_reauth"].contains(profile.recoveryReason ?? "") { return true }
        guard let error = profile.lastError else { return false }
        return error.contains("注册有效期") || error.contains("有效期即将结束")
    }

    private func isRecovering(_ profile: MultiStandbyProfile) -> Bool {
        ["degraded", "backoff"].contains(profile.health ?? "")
            || ["backoff", "authenticating"].contains(profile.authState ?? "")
    }

    private func runtimeDetail(_ profile: MultiStandbyProfile) -> String {
        var lines: [String] = []
        if let reason = profile.recoveryReason { lines.append(MultiStandbyPresentation.reason(reason)) }
        if let time = profile.registrationRenewAtMS, time > 0 { lines.append("计划刷新 " + runtimeTime(time)) }
        if let time = profile.registrationExpiresAtMS, time > 0 { lines.append("IMS 租期至 " + runtimeTime(time)) }
        if let time = profile.nextRetryAtMS, time > 0 { lines.append("下次重试 " + runtimeTime(time)) }
        if let failures = profile.consecutiveFailures, failures > 0 { lines.append("连续失败 \(failures) 次") }
        if let error = profile.lastError, !error.isEmpty { lines.append(error) }
        return lines.joined(separator: "\n")
    }

    private func runtimeTime(_ milliseconds: Int64) -> String {
        Date(timeIntervalSince1970: Double(milliseconds) / 1000).formatted(date: .omitted, time: .standard)
    }

    private var runtimeReportView: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Label("运行记录", systemImage: "clock.arrow.circlepath").font(.title3.weight(.semibold))
                Spacer()
                Picker("统计窗口", selection: $reportHours) {
                    Text("近 1 小时").tag(1); Text("近 12 小时").tag(12)
                    Text("近 24 小时").tag(24); Text("近 7 天").tag(168)
                }.frame(width: 128)
                Button { Task { await store.refreshRuntimeReport(hours: reportHours) } } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.proletarius).disabled(store.runtimeReportLoading)
                Button("完成") { showingReport = false }.buttonStyle(.proletarius)
            }
            if store.runtimeReportLoading { ProgressView().controlSize(.small) }
            if let error = store.runtimeReportError { Text(error).font(.caption).foregroundStyle(.orange) }
            if let report = store.runtimeReport {
                Text("\(Date(timeIntervalSince1970: Double(report.sinceMS) / 1000).formatted()) — \(Date(timeIntervalSince1970: Double(report.untilMS) / 1000).formatted())")
                    .font(.caption).foregroundStyle(.secondary)
                Text("在线以短信通道就绪为准；IMS 续期、计划重认证与意外掉线分开统计。历史缺失、服务进程间隔、观测暂停超过一分钟和时钟调整计为未知，不能据此判断运营商套餐到期。")
                    .font(.caption).foregroundStyle(.secondary)
                if report.storageIssue != nil { Text("运行记录保存异常；当前报告可能不完整。").font(.caption).foregroundStyle(.orange) }
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        if report.profiles.isEmpty { Text("当前号卡尚无运行记录。").foregroundStyle(.secondary) }
                        ForEach(report.profiles) { item in
                            VStack(alignment: .leading, spacing: 6) {
                                let profile = store.profiles.first { $0.profileID == item.profileID }
                                Text(profile?.label(masked: privacyMaskEnabled) ?? "线路 · " + item.profileID.suffix(4))
                                    .font(.system(size: 12, weight: .semibold))
                                HStack(spacing: 14) {
                                    reportMetric("在线", MultiStandbyPresentation.duration(item.onlineMS), color: .green)
                                    reportMetric("未就绪", MultiStandbyPresentation.duration(item.notReadyMS), color: .orange)
                                    reportMetric("未知", MultiStandbyPresentation.duration(item.unknownMS), color: .secondary)
                                    reportMetric("IMS 续期", "\(item.plannedRenewals) 次", color: WorkspaceColors.accent)
                                    reportMetric("计划重认证", "\(item.plannedIKEReauths) 次", color: WorkspaceColors.accent)
                                    reportMetric("意外故障", "\(item.unexpectedFailures) 次", color: .orange)
                                    reportMetric("最慢恢复", MultiStandbyPresentation.duration(item.slowestRecoveryMS), color: .secondary)
                                }
                                let total = max(1, report.untilMS - report.sinceMS)
                                Text("观测覆盖 \(Int(Double(max(0, total - item.unknownMS)) / Double(total) * 100))% · 手动重连 \(item.manualRetries) 次 · 已停止 \(MultiStandbyPresentation.duration(item.stoppedMS))")
                                    .font(.system(size: 10)).foregroundStyle(.secondary)
                                if let profile { Text(runtimeDetail(profile)).font(.system(size: 10)).foregroundStyle(.secondary) }
                            }.padding(10).frame(maxWidth: .infinity, alignment: .leading).workspaceCard()
                        }
                        Text("最近事件（最多显示 80 条）").font(.headline).padding(.top, 6)
                        ForEach(Array(report.events.suffix(80).reversed())) { event in
                            HStack(spacing: 10) {
                                Text(runtimeTime(event.atMS)).font(.system(size: 10, design: .monospaced))
                                Text(store.profiles.first(where: { $0.profileID == event.profileID })?.title ?? "线路 · " + event.profileID.suffix(4))
                                    .font(.caption).lineLimit(1)
                                Spacer()
                                Text(MultiStandbyPresentation.reason(event.reason)).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        Text("最多保存 \(report.retentionDays) 天、\(report.eventLimit) 条状态事件；超过记录范围的时间显示为未知。")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                }
            } else if !store.runtimeReportLoading { Text("尚未生成统计。").foregroundStyle(.secondary); Spacer() }
        }
        .padding(18).frame(width: 780, height: 560)
        .onChange(of: reportHours) { _, _ in Task { await store.refreshRuntimeReport(hours: reportHours) } }
    }

    private func reportMetric(_ title: String, _ value: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.system(size: 10)).foregroundStyle(.secondary)
            Text(value).font(.system(size: 11, weight: .medium).monospacedDigit()).foregroundStyle(color)
        }
    }

    private func stateTitle(_ state: String?) -> String {
        switch state {
        case "idle", "stopped", "disconnected": return "未连接"
        case "queued", "waiting": return "排队中"
        case "connecting", "registering", "authenticating": return "连接中"
        case "backoff", "degraded": return "恢复中"
        case "offline": return "掉线"
        case "verified", "authenticated", "connected", "registered", "ready": return "就绪"
        case "failed", "error": return "失败"
        case "unknown", nil: return "待确认"
        default: return state ?? "待确认"
        }
    }
}

private struct PageLayout {
    let columns: [GridItem]
    let pageSize: Int
    let pageCount: Int
    let pageIndex: Int
    let cardHeight: CGFloat
    let pageInset: CGFloat
    let verticalInset: CGFloat
    let spacing: CGFloat
    let compactControls: Bool
}

private struct StatusBanner {
    let title: String
    let detail: String?
    let symbol: String
    let color: Color
    var showsProgress = false
}

private struct NWaitHeaderMark: View {
    var body: some View {
        Image("NWaitLoop")
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(width: WorkspaceHeaderMetrics.iconFrame,
                   height: WorkspaceHeaderMetrics.iconFrame)
            .accessibilityLabel("N待线路标识")
    }
}
