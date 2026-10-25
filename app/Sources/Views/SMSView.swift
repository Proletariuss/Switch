import SwiftUI

/// One inbox for module/reader messages and every N待 line.
struct SMSView: View {
    @EnvironmentObject private var smsStore: SMSStore
    @EnvironmentObject private var attentionStore: AttentionStore
    @EnvironmentObject private var dashboardStore: DashboardStore
    @EnvironmentObject private var multiStandbyStore: MultiStandbyStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @StateObject private var forwardingAccess = SMSForwardingViewAccess()
    @ObservedObject private var deviceSends = DeviceSMSSendStore.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.workspaceViewport) private var viewport

    @State private var selectedThreadID: String?
    @State private var focusedMessageID: String?
    @State private var searchText = ""
    @State private var selectedItem: SMSItem?
    @State private var deleteTarget: SMSDeleteTarget?
    @State private var deleting = false
    @State private var deleteError: String?
    @State private var showClearOptions = false
    @State private var showForwardingSettings = false
    @State private var showExport = false

    private var exportProfiles: [SMSExportProfileOption] {
        var seen = Set<String>()
        return (multiStandbyStore.inboxProfiles + multiStandbyStore.profiles).compactMap { profile in
            guard seen.insert(profile.profileID).inserted else { return nil }
            return SMSExportProfileOption(id: profile.profileID, title: profile.label(masked: privacyMaskEnabled))
        }
    }
    private var exportSubscriptions: [SMSExportSubscriptionOption] {
        var seen = Set<String>()
        let entries = smsStore.items.compactMap { item -> (String, String)? in
            item.subscription.map { (item.lineID, $0) }
        } + smsStore.devices.compactMap { device -> (String, String)? in
            device.subscription.map { (device.id, $0) }
        }
        return entries.compactMap { device, subscription in
            guard seen.insert(device + "|" + subscription).inserted else { return nil }
            let label = SMSDeviceStatus.historyLabel(lineID: device, subscriptionID: subscription,
                devices: smsStore.devices, masked: privacyMaskEnabled)
            return SMSExportSubscriptionOption(id: subscription, title: label + " · \(seen.count)", device: device)
        }
    }

    private var allThreads: [SMSCenterThread] {
        let deviceConversations = Dictionary(grouping: smsStore.items, by: \.threadID)
            .compactMap { _, messages -> Conversation? in
                guard let first = messages.first else { return nil }
                return Conversation(phone: first.sender ?? "未知号码",
                                    messages: messages.sorted { $0.timestamp < $1.timestamp }, lineID: first.lineID)
            }
            .map { conversation in
                let line = SMSDeviceStatus.historyLabel(lineID: conversation.lineID,
                    subscriptionID: conversation.subscriptionID, devices: smsStore.devices, masked: privacyMaskEnabled)
                return SMSCenterThread.device(conversation, line: line)
            }

        let nwaitConversations = multiStandbyStore.inboxConversations.map { conversation in
            let profile = multiStandbyStore.inboxProfiles.first { $0.profileID == conversation.profileID }
            return SMSCenterThread.multiStandby(conversation, line: profile?.title)
        }

        return (deviceConversations + nwaitConversations).sorted {
            if $0.latestDate != $1.latestDate { return $0.latestDate > $1.latestDate }
            return $0.contact.localizedStandardCompare($1.contact) == .orderedAscending
        }
    }

    private var visibleThreads: [SMSCenterThread] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return allThreads }
        return allThreads.filter {
            $0.contact.localizedCaseInsensitiveContains(query)
                || ($0.sourceLabel ?? "").localizedCaseInsensitiveContains(query)
                || $0.preview.localizedCaseInsensitiveContains(query)
        }
    }

    private var selectedThread: SMSCenterThread? {
        guard let selectedThreadID else { return nil }
        return allThreads.first { $0.id == selectedThreadID }
    }

    private var usesConversationColumns: Bool {
        viewport.contentWidth >= 230 + 360 + viewport.sectionSpacing
    }

    private var conversationLayout: AnyLayout {
        usesConversationColumns
            ? AnyLayout(HStackLayout(alignment: .top, spacing: viewport.sectionSpacing))
            : AnyLayout(VStackLayout(alignment: .leading, spacing: viewport.sectionSpacing))
    }

    private var conversationListWidth: CGFloat? {
        guard usesConversationColumns else { return nil }
        return min(320, max(230, viewport.contentWidth * 0.31))
    }

    private var notificationBinding: Binding<Bool> {
        Binding(
            get: { smsStore.notificationsEnabled && multiStandbyStore.notificationsEnabled },
            set: { enabled in
                smsStore.notificationsEnabled = enabled
                multiStandbyStore.notificationsEnabled = enabled
            })
    }

    var body: some View {
        dialogContent
    }

    private var pageContent: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            WorkspaceHero(
                title: "短信中心",
                subtitle: "所有线路的短信对话集中查看与收发。",
                symbol: "message.fill",
                eyebrow: "MESSAGES")
            centerToolbar
            DeviceSMSRecoveryView()
            conversationLayout {
                conversationSidebar
                    .frame(width: conversationListWidth,
                           height: usesConversationColumns ? nil : min(230, viewport.height * 0.30))
                conversationDetail
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .frame(maxHeight: .infinity)
        }
        .padding(viewport.pageInset)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.18), value: selectedThreadID)
        .buttonStyle(.proletarius)
    }

    private var lifecycleContent: some View {
        pageContent
        .onAppear {
            smsStore.viewingSMS = true
            multiStandbyStore.viewingInbox = true
            multiStandbyStore.viewingPage = true
            updateViewedConversation()
            smsStore.refresh()
            if multiStandbyStore.shouldOpenInbox { multiStandbyStore.consumeSMSInboxRequest() }
            Task { await multiStandbyStore.refreshSMSCenter() }
            Task { await DeviceSMSSendStore.shared.refresh() }
            openPendingConversation()
            openRequestedMultiStandbyMessage()
        }
        .onDisappear {
            smsStore.viewingSMS = false
            multiStandbyStore.viewingInbox = false
            multiStandbyStore.viewingPage = false
            attentionStore.setViewingSMS(threadID: nil)
        }
    }

    private var deviceObservationContent: some View {
        lifecycleContent
        .compatibleOnChange(of: deviceSends.available) { ready in
            if ready { Task { await deviceSends.refresh() } }
        }
        .compatibleOnChange(of: smsStore.pendingOpenSender) { _ in openPendingConversation() }
        .compatibleOnChange(of: smsStore.pendingOpenLineID) { _ in openPendingConversation() }
        .compatibleOnChange(of: smsStore.pendingOpenMessageID) { _ in openPendingConversation() }
        .compatibleOnChange(of: smsStore.pendingOpenSubscriptionID) { _ in openPendingConversation() }
        .compatibleOnChange(of: selectedThreadID) { _ in updateViewedConversation() }
        .compatibleOnChange(of: selectedThread?.id) { _ in updateViewedConversation() }
        .compatibleOnChange(of: smsStore.items.map(\.notificationID)) { _ in
            openPendingConversation()
            updateViewedConversation()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            updateViewedConversation()
        }
    }

    private var inboxObservationContent: some View {
        deviceObservationContent
        .compatibleOnChange(of: multiStandbyStore.inboxMessages) { _ in
            openRequestedMultiStandbyMessage()
            updateViewedConversation()
        }
        .compatibleOnChange(of: multiStandbyStore.inboxConversations) { _ in
            openRequestedMultiStandbyMessage()
            updateViewedConversation()
        }
        .compatibleOnChange(of: multiStandbyStore.selectedMessageID) { _ in openRequestedMultiStandbyMessage() }
        .compatibleOnChange(of: multiStandbyStore.messages) { _ in openRequestedMultiStandbyMessage() }
    }

    private var sheetContent: some View {
        inboxObservationContent
        .sheet(item: $selectedItem) { item in
            SMSDetailView(item: item, onDelete: { deleted in
                if deleted {
                    selectedItem = nil
                    smsStore.refresh(force: true)
                }
            }, onReply: {
                selectedItem = nil
                selectedThreadID = item.threadID
                smsStore.refresh(force: true)
            })
        }
        .sheet(isPresented: $showClearOptions) {
            SMSClearView(
                readerMode: dashboardStore.status?.networkMode == "reader",
                onDone: { smsStore.refresh(force: true) })
        }
        .sheet(isPresented: $showForwardingSettings) {
            if let store = forwardingAccess.store {
                SMSForwardingSettingsView(store: store)
            } else {
                Text("隔离验收中不可用").padding()
            }
        }
        .sheet(isPresented: $showExport) {
            SMSExportView(profiles: exportProfiles, subscriptions: exportSubscriptions)
        }
    }

    private var dialogContent: some View {
        sheetContent
        .confirmationDialog(
            "确认删除？",
            isPresented: Binding(
                get: { deleteTarget != nil },
                set: { if !$0 { deleteTarget = nil } }),
            titleVisibility: .visible,
            presenting: deleteTarget
        ) { target in
            deletionConfirmationActions(for: target)
        } message: { target in
            switch target {
            case .message:
                Text("这条短信将从实际存储位置删除，此操作不可撤销。")
            case .conversation(let conversation):
                Text("将删除此线路与 \(conversation.phone) 的 \(conversation.messages.count) 条短信，包括实际设备存储与本机归档。此操作不可撤销。")
            }
        }
        .alert("删除失败", isPresented: Binding(
            get: { deleteError != nil }, set: { if !$0 { deleteError = nil } })) {
            Button("好", role: .cancel) { deleteError = nil }
        } message: {
            Text(deleteError ?? "未知错误")
        }
    }

    @ViewBuilder
    private func deletionConfirmationActions(for target: SMSDeleteTarget) -> some View {
        switch target {
        case .message(let item):
            Button("删除此短信", role: ButtonRole.destructive) {
                performDelete(SMSDeleteTarget.message(item))
            }
            .disabled(deleting)
        case .conversation(let conversation):
            Button("删除此线路全部 \(conversation.messages.count) 条短信", role: ButtonRole.destructive) {
                performDelete(SMSDeleteTarget.conversation(conversation))
            }
            .disabled(deleting)
        }
        Button("取消", role: ButtonRole.cancel) {}
    }

    private var centerToolbar: some View {
        HStack(spacing: 12) {
            Text("\(allThreads.count) 段对话")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
            Spacer(minLength: 8)
            Toggle("新短信通知", isOn: notificationBinding)
                .toggleStyle(.checkbox)
                .font(.caption)
            Button {
                smsStore.refresh(force: true)
                Task { await multiStandbyStore.refreshSMSCenter() }
                Task { await deviceSends.refresh() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.proletariusToolbar)
            .help("刷新短信中心")
            .disabled(smsStore.busy || multiStandbyStore.refreshing)
            Button { showForwardingSettings = true } label: {
                Image(systemName: "arrowshape.turn.up.right")
                    .foregroundStyle(WorkspaceColors.accent)
                    .frame(width: ProletariusLayout.toolbarButtonFrame,
                           height: ProletariusLayout.toolbarButtonFrame)
            }
            .buttonStyle(.proletariusToolbar)
            .help("短信转发设置")
            Button { showExport = true } label: {
                Image(systemName: "square.and.arrow.up")
                    .frame(width: ProletariusLayout.toolbarButtonFrame,
                           height: ProletariusLayout.toolbarButtonFrame)
            }
            .buttonStyle(.proletariusToolbar)
            .help("导出短信归档")
            Menu {
                Button("全部标为已读") { attentionStore.markSMSViewed() }
                    .disabled(attentionStore.unreadSMSCount == 0)
                Button("清空短信…", role: .destructive) { showClearOptions = true }
            } label: {
                Image(systemName: "ellipsis")
                    .frame(width: ProletariusLayout.toolbarButtonFrame,
                           height: ProletariusLayout.toolbarButtonFrame)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .buttonStyle(.proletariusToolbar)
            .help("清理短信")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .workspaceCard()
    }

    private var conversationSidebar: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("对话", systemImage: "bubble.left.and.bubble.right.fill")
                    .font(.headline)
                Spacer(minLength: 0)
                Button { selectedThreadID = nil; focusedMessageID = nil } label: { Image(systemName: "square.and.pencil") }
                    .buttonStyle(.proletariusToolbar)
                    .help("新建对话")
                    .accessibilityLabel("新建对话")
            }
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("搜索短信", text: $searchText)
                    .textFieldStyle(.plain)
                    .help("搜索联系人、线路或短信正文")
                if !searchText.isEmpty {
                    Button { searchText = "" } label: {
                        Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("清除搜索")
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(ProletariusPalette.action.opacity(0.07), in: RoundedRectangle(cornerRadius: 10))
            if visibleThreads.isEmpty {
                ContentUnavailableView(
                    searchText.isEmpty ? "暂无对话" : "没有匹配的短信",
                    systemImage: searchText.isEmpty ? "message" : "magnifyingglass",
                    description: Text(searchText.isEmpty ? "收到或发送的短信会集中显示在这里。" : "试试联系人号码或短信内容。"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 5) {
                        ForEach(visibleThreads) { thread in
                            SMSCenterThreadRow(thread: thread, isSelected: selectedThreadID == thread.id) {
                                selectedThreadID = thread.id
                                focusedMessageID = nil
                            }
                            .contextMenu {
                                if case .device(let conversation) = thread.source,
                                   conversation.senderForDeletion != nil {
                                    Button("删除该号码的全部短信…", role: .destructive) {
                                        deleteTarget = .conversation(conversation)
                                    }
                                }
                            }
                        }
                    }
                }
                .scrollIndicators(.visible)
                .scrollBounceBehavior(.basedOnSize)
            }
        }
        .padding(12)
        .frame(maxHeight: .infinity, alignment: .topLeading)
        .workspaceCard()
    }

    @ViewBuilder
    private var conversationDetail: some View {
        if let thread = selectedThread {
            switch thread.source {
            case .device(let conversation):
                ConversationDetailView(
                    conversation: conversation,
                    focusMessageID: focusedMessageID,
                    onOpenMessage: { selectedItem = $0 },
                    onDeleteMessage: { deleteTarget = .message($0) },
                    onDeleteConversation: { _, _ in
                        deleteTarget = .conversation(conversation)
                    },
                    onSent: { smsStore.refresh(force: true) })
                    .id(conversation.id)
            case .multiStandby(let profileID, let peer):
                SMSCenterMultiStandbyDetailView(profileID: profileID, peer: peer, focusMessageID: focusedMessageID)
                    .id(thread.id)
                    .environmentObject(multiStandbyStore)
            }
        } else {
            SMSCenterNewMessageView()
                .environmentObject(smsStore)
                .environmentObject(multiStandbyStore)
        }
    }

    private func openPendingConversation() {
        guard let sender = smsStore.pendingOpenSender else { return }
        if let messageID = smsStore.pendingOpenMessageID,
           let item = smsStore.items.first(where: { ($0.backendID ?? $0.notificationID) == messageID
               && (smsStore.pendingOpenLineID == nil || $0.lineID == smsStore.pendingOpenLineID)
               && (smsStore.pendingOpenSubscriptionID == nil || $0.subscription == smsStore.pendingOpenSubscriptionID) }) {
            selectedThreadID = item.threadID
            focusedMessageID = item.id
        } else if let lineID = smsStore.pendingOpenLineID {
            selectedThreadID = SMSConversationIdentity.device(lineID: lineID, peer: sender, subscriptionID: smsStore.pendingOpenSubscriptionID)
        } else {
            // A legacy notification has no subscription evidence. It may open
            // only legacy history, never choose a currently inserted SIM.
            selectedThreadID = smsStore.items.first { $0.sender == sender && $0.subscription == nil }?.threadID
        }
        guard selectedThreadID != nil else { return }
        smsStore.consumeOpenRequest()
    }

    private func openRequestedMultiStandbyMessage() {
        guard let messageID = multiStandbyStore.selectedMessageID,
              let message = (multiStandbyStore.inboxMessages + multiStandbyStore.messages).first(where: {
                  $0.messageID == messageID && $0.profileID == multiStandbyStore.selectedProfileID
              }) else { return }
        selectedThreadID = SMSConversationIdentity.multiStandby(profileID: message.profileID, peer: message.peer)
        focusedMessageID = message.id
        multiStandbyStore.consumeOpenMessageRequest()
    }

    private func updateViewedConversation() {
        attentionStore.setViewingSMS(threadID: selectedThread?.id)
    }

    private func performDelete(_ target: SMSDeleteTarget) {
        guard !deleting else { return }
        deleting = true
        deleteTarget = nil
        Task {
            do {
                switch target {
                case .message(let item):
                    _ = try await smsStore.deleteMessage(item)
                    if selectedItem?.id == item.id { selectedItem = nil }
                case .conversation(let conversation):
                    _ = try await smsStore.deleteConversation(conversation.messages)
                    if selectedThreadID == conversation.id { selectedThreadID = nil }
                }
                deleting = false
                smsStore.refresh(force: true)
            } catch {
                deleting = false
                deleteError = error.localizedDescription
            }
        }
    }
}

/// Retains the same production singleton at view construction, without even
/// constructing its Keychain/real-user-lock authority in the acceptance lane.
@MainActor
private final class SMSForwardingViewAccess: ObservableObject {
    let store: SMSForwardingStore? = AcceptanceBootstrap.isAcceptance ? nil : .shared
}

private enum SMSDeleteTarget: Identifiable {
    case message(SMSItem)
    case conversation(Conversation)

    var id: String {
        switch self {
        case .message(let item): return "message|\(item.id)"
        case .conversation(let conversation): return conversation.id
        }
    }
}
