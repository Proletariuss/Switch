import SwiftUI

struct SMSCenterThread: Identifiable {
    enum Source {
        case device(Conversation)
        case multiStandby(profileID: String, peer: String)
    }

    let id: String
    let contact: String
    let sourceLabel: String?
    let preview: String
    let latestDate: Date
    let messageCount: Int
    let outgoing: Bool
    let source: Source

    static func device(_ conversation: Conversation, line: String?) -> SMSCenterThread {
        let last = conversation.lastMessage
        return SMSCenterThread(
            id: conversation.id,
            contact: conversation.phone,
            sourceLabel: line,
            preview: last?.content ?? "",
            latestDate: conversation.lastTimestamp,
            messageCount: conversation.messages.count,
            outgoing: last?.isOutgoing == true,
            source: .device(conversation))
    }

    static func multiStandby(_ conversation: MultiStandbyConversation, line: String?) -> SMSCenterThread {
        let last = conversation.latestMessage
        return SMSCenterThread(
            id: SMSConversationIdentity.multiStandby(profileID: conversation.profileID, peer: conversation.peer),
            contact: conversation.peer,
            sourceLabel: line,
            preview: last.body,
            latestDate: last.date,
            messageCount: conversation.messageCount,
            outgoing: last.isOutgoing,
            source: .multiStandby(profileID: conversation.profileID, peer: conversation.peer))
    }
}

struct SMSCenterThreadRow: View {
    let thread: SMSCenterThread
    let isSelected: Bool
    let action: () -> Void
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @EnvironmentObject private var attentionStore: AttentionStore

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: "person.crop.circle.fill")
                    .font(.system(size: 25))
                    .foregroundStyle(WorkspaceColors.accent.opacity(0.82))
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(PrivacyMask.value(thread.contact, enabled: privacyMaskEnabled))
                            .font(.system(size: 13, weight: .semibold))
                            .lineLimit(1)
                        Spacer(minLength: 2)
                        if attentionStore.unreadCount(threadID: thread.id) > 0 {
                            AttentionBadge(count: attentionStore.unreadCount(threadID: thread.id), accessibilityName: "未读短信")
                        }
                        Text(thread.latestDate.formatted(date: .omitted, time: .shortened))
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(.tertiary)
                    }
                    if let sourceLabel = thread.sourceLabel, !sourceLabel.isEmpty {
                        Text(sourceLabel)
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                    }
                    HStack(spacing: 4) {
                        if thread.outgoing { Text("我：") }
                        Text(privacyMaskEnabled ? "内容已隐藏" : thread.preview)
                            .lineLimit(1)
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(isSelected ? WorkspaceColors.accent.opacity(0.11) : .clear,
                        in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("查看与 \(thread.contact) 的短信")
    }
}

struct SMSCenterMultiStandbyDetailView: View {
    @Environment(\.workspaceViewport) private var viewport
    @EnvironmentObject private var store: MultiStandbyStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    let profileID: String
    let peer: String
    var focusMessageID: String? = nil

    @State private var draft = ""
    @State private var deleting: MultiStandbyMessage?
    @State private var didFocusMessage = false

    private var canSendOnLine: Bool { store.isServiceRunning && profile?.isReady == true && !store.sending }

    private var profile: MultiStandbyProfile? {
        store.inboxProfiles.first { $0.profileID == profileID }
            ?? store.profiles.first { $0.profileID == profileID }
    }

    private var conversation: [MultiStandbyMessage] {
        store.messages.filter { $0.profileID == profileID && $0.peer == peer }.sorted {
            $0.createdAtMS == $1.createdAtMS ? $0.messageID < $1.messageID : $0.createdAtMS < $1.createdAtMS
        }
    }
    private var totalCount: Int {
        store.inboxConversations.first { $0.profileID == profileID && $0.peer == peer }?.messageCount ?? conversation.count
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 9) {
                Image(systemName: "person.crop.circle.fill")
                    .font(.system(size: 25)).foregroundStyle(WorkspaceColors.accent)
                VStack(alignment: .leading, spacing: 3) {
                    Text(PrivacyMask.value(peer, enabled: privacyMaskEnabled)).font(.headline)
                    Text("\(totalCount) 条短信 · 已加载 \(conversation.count) 条 · \(profile?.label(masked: privacyMaskEnabled) ?? "短信线路")")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer()
            }
            Divider()
            if conversation.isEmpty && store.messagesLoading {
                ProgressView("正在同步短信")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if conversation.isEmpty {
                ContentUnavailableView("暂无短信", systemImage: "message",
                                       description: Text("可以在下方直接发送一条短信。"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 10) {
                            if store.messageCursor != nil {
                                Button {
                                    let anchor = conversation.first?.id
                                    Task {
                                        await store.refreshMessages(loadMore: true)
                                        await Task.yield()
                                        if let anchor { proxy.scrollTo(anchor, anchor: .top) }
                                    }
                                } label: {
                                    if store.messagesLoading { ProgressView().controlSize(.small) }
                                    else { Label("加载更早短信", systemImage: "clock.arrow.circlepath") }
                                }
                                .font(.caption)
                                .disabled(store.messagesLoading)
                                .frame(maxWidth: .infinity)
                            }
                            ForEach(conversation) { message in messageRow(message) }
                        }
                        .padding(.vertical, 8)
                    }
                    .onChange(of: conversation.last?.id) { _, id in
                        if !didFocusMessage, let focusMessageID, conversation.contains(where: { $0.id == focusMessageID }) {
                            proxy.scrollTo(focusMessageID, anchor: .center)
                            didFocusMessage = true
                            return
                        }
                        if let id { withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo(id, anchor: .bottom) } }
                    }
                    .onAppear {
                        if let focusMessageID, conversation.contains(where: { $0.id == focusMessageID }) {
                            proxy.scrollTo(focusMessageID, anchor: .center)
                            didFocusMessage = true
                            return
                        }
                        if let id = conversation.last?.id { proxy.scrollTo(id, anchor: .bottom) }
                    }
                    .onChange(of: focusMessageID) { _, id in
                        if let id, conversation.contains(where: { $0.id == id }) {
                            proxy.scrollTo(id, anchor: .center)
                            didFocusMessage = true
                        }
                    }
                }
            }
            Divider()
            SMSCenterSendRecoveryView(profileID: profileID, peer: peer)
            HStack(spacing: 8) {
                TextField("输入短信内容", text: $draft)
                    .textFieldStyle(.workspace)
                    .onSubmit(send)
                Button {
                    send()
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.up")
                        Text("发送")
                    }
                }
                .buttonStyle(.proletariusPrimary)
                .disabled(!canSendOnLine || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            if let notice = store.sendNotice, !notice.isEmpty {
                Label(notice, systemImage: "checkmark.circle")
                    .font(.caption).foregroundStyle(.secondary)
            } else if !canSendOnLine {
                Text("此线路暂不可发送短信。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let error = store.actionError, !error.isEmpty {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption).foregroundStyle(.orange).lineLimit(2)
            }
        }
        .padding(12)
        .frame(maxHeight: .infinity, alignment: .topLeading)
        .task(id: SMSConversationIdentity.multiStandby(profileID: profileID, peer: peer)) {
            await store.refreshSendIntents()
            if store.selectedProfileID != profileID || store.selectedPeer != peer {
                store.selectConversation(profileID: profileID, peer: peer)
            } else {
                await store.refreshMessages()
            }
        }
        .alert("删除这条短信？", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button("取消", role: .cancel) { deleting = nil }
            Button("删除", role: .destructive) {
                if let message = deleting { Task { await store.delete(message) } }
                deleting = nil
            }
        } message: {
            Text("只删除当前号码的这条短信记录。")
        }
    }

    private func messageRow(_ message: MultiStandbyMessage) -> some View {
        HStack {
            if message.isOutgoing { Spacer(minLength: 56) }
            VStack(alignment: message.isOutgoing ? .trailing : .leading, spacing: 5) {
                Text(message.body)
                    .font(.callout).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: message.isOutgoing ? .trailing : .leading)
                HStack(spacing: 7) {
                    Text(message.date.formatted(date: .abbreviated, time: .shortened))
                    Text(MultiStandbyStore.messageState(message.state))
                    Button { deleting = message } label: { Image(systemName: "trash") }
                        .buttonStyle(.plain).help("删除短信")
                        .disabled(!store.isServiceRunning)
                }
                .font(.caption2).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12).padding(.vertical, 9)
            .frame(maxWidth: 460, alignment: message.isOutgoing ? .trailing : .leading)
            .background(message.isOutgoing ? WorkspaceColors.accent.opacity(0.12) : Color.secondary.opacity(0.07),
                        in: RoundedRectangle(cornerRadius: 12))
            if !message.isOutgoing { Spacer(minLength: 56) }
        }
        .id(message.id)
    }

    private func send() {
        let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, canSendOnLine else { return }
        Task {
            if await store.send(phone: peer, body: body, expectedProfileID: profileID), draft.trimmingCharacters(in: .whitespacesAndNewlines) == body {
                draft = ""
            }
        }
    }
}

struct SMSCenterNewMessageView: View {
    @Environment(\.workspaceViewport) private var viewport
    @EnvironmentObject private var smsStore: SMSStore
    @EnvironmentObject private var multiStandbyStore: MultiStandbyStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    @State private var selectedRouteID = ""
    @State private var recipient = ""
    @State private var draft = ""
    @State private var sending = false
    @State private var sendError: String?

    private struct Route: Identifiable {
        enum Kind { case device(String, String?), multiStandby(String) }
        let id: String
        let title: String
        let kind: Kind
    }

    private var routes: [Route] {
        let devices = smsStore.readySendDevices.map { device in
            Route(id: "device|" + device.selectionID,
                  title: device.displayName(privacyMaskEnabled: privacyMaskEnabled),
                  kind: .device(device.id, device.subscription))
        }
        let profiles = multiStandbyStore.inboxProfiles.filter(\.isReady).map { profile in
            Route(id: "nwait|" + profile.profileID,
                  title: profile.label(masked: privacyMaskEnabled),
                  kind: .multiStandby(profile.profileID))
        }
        return devices + profiles
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SMSCenterSendRecoveryView()
            if routes.isEmpty {
                ContentUnavailableView("没有可用短信线路", systemImage: "simcard",
                                       description: Text("连接并启用一条短信线路后，即可在这里发送短信。"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                HStack(spacing: 10) {
                    WorkspaceSymbol(systemName: "bubble.left.and.bubble.right.fill",
                                    tint: WorkspaceColors.accent, size: 34)
                    WorkspaceSectionHeader(title: "新建短信", detail: "选择线路并填写收件人")
                }
                Picker("发送线路", selection: $selectedRouteID) {
                    Text("请选择发送线路").tag("")
                    ForEach(routes) { route in Text(route.title).tag(route.id) }
                }
                .pickerStyle(.menu)
                .disabled(sending)
                TextField("收件人号码", text: $recipient)
                    .textFieldStyle(.workspace)
                    .disabled(sending)
                TextField("输入短信内容", text: $draft, axis: .vertical)
                    .textFieldStyle(.workspace)
                    .lineLimit(3...7)
                    .disabled(sending)
                if let sendError {
                    Text(sendError).font(.caption).foregroundStyle(.red)
                }
                HStack {
                    Spacer()
                    Button(action: send) {
                        if sending { ProgressView().controlSize(.small) }
                        else { Label("发送", systemImage: "arrow.up") }
                    }
                    .buttonStyle(.proletariusPrimary)
                    .disabled(sending || selectedRouteID.isEmpty
                        || recipient.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .padding(viewport.cardPadding)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear(perform: reconcileRoute)
        .task { await multiStandbyStore.refreshSendIntents() }
        .compatibleOnChange(of: routes.map(\.id)) { _ in reconcileRoute() }
        .compatibleOnChange(of: selectedRouteID) { id in
            guard let route = routes.first(where: { $0.id == id }) else { return }
            if case .device(let deviceID, _) = route.kind { smsStore.selectedSendDevice = deviceID }
            if case .multiStandby(let profileID) = route.kind,
               multiStandbyStore.selectedProfileID != profileID {
                multiStandbyStore.selectProfile(profileID)
            }
        }
    }

    private func reconcileRoute() {
        guard !routes.isEmpty else { selectedRouteID = ""; return }
        if routes.contains(where: { $0.id == selectedRouteID }) { return }
        if !recipient.isEmpty || !draft.isEmpty { selectedRouteID = ""; return }
        if let device = routes.first(where: {
            if case .device(let id, _) = $0.kind { return id == smsStore.selectedSendDevice }
            return false
        }) {
            selectedRouteID = device.id
        } else {
            selectedRouteID = routes[0].id
        }
    }

    private func send() {
        guard let route = routes.first(where: { $0.id == selectedRouteID }) else { return }
        let phone = recipient.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !phone.isEmpty, !body.isEmpty, !sending else { return }
        sending = true
        sendError = nil
        Task {
            do {
                switch route.kind {
                case .device(let deviceID, let subscription):
                    smsStore.selectedSendDevice = deviceID
                    _ = try await DeviceSMSSendStore.shared.send(phone: phone, message: body,
                        device: deviceID, subscriptionID: subscription)
                    smsStore.refresh(force: true)
                case .multiStandby(let profileID):
                    guard await multiStandbyStore.send(phone: phone, body: body, expectedProfileID: profileID) else {
                        throw NSError(domain: "SMSCenter", code: 1,
                                      userInfo: [NSLocalizedDescriptionKey: multiStandbyStore.actionError ?? "短信发送失败"])
                    }
                }
                await MainActor.run {
                    sending = false
                    recipient = ""
                    draft = ""
                }
            } catch {
                await MainActor.run {
                    sending = false
                    sendError = smsSendErrorMessage(error)
                    if case .device = route.kind, operationOutcomeIsUnknown(error) {
                        smsStore.refresh()
                    }
                }
            }
        }
    }
}


/// Recovery actions always own their original profile and payload. Unknown
/// results offer a read and a separately confirmed fresh request, never replay.
struct SMSCenterSendRecoveryView: View {
    @EnvironmentObject private var store: MultiStandbyStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    var profileID: String? = nil
    var peer: String? = nil
    @State private var newSend: MultiStandbySendIntent?

    private var intents: [MultiStandbySendIntent] {
        store.unresolvedSends.filter { (profileID == nil || $0.profileID == profileID) && (peer == nil || $0.phone == peer) }
    }
    private func ready(_ intent: MultiStandbySendIntent) -> Bool {
        store.isServiceRunning && !store.sending
            && (store.inboxProfiles + store.profiles).contains { $0.profileID == intent.profileID && $0.isReady }
    }
    private func lineName(_ intent: MultiStandbySendIntent) -> String {
        (store.inboxProfiles + store.profiles).first { $0.profileID == intent.profileID }?.label(masked: privacyMaskEnabled) ?? "原发送线路"
    }
    var body: some View {
        if !intents.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Label("未完成发送", systemImage: "clock.arrow.circlepath").font(.caption.weight(.semibold))
                    Spacer()
                    Button("查询记录") { Task { await store.refreshSendIntents() } }
                        .disabled(!store.isServiceRunning || store.sendRecoveryLoading)
                        .font(.caption)
                }
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 6) {
                        ForEach(intents) { intent in
                            HStack(spacing: 8) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(PrivacyMask.value(intent.phone, enabled: privacyMaskEnabled)).lineLimit(1)
                                    Text(lineName(intent) + " · " + MultiStandbyStore.messageState(intent.state))
                                        .foregroundStyle(.secondary).lineLimit(1)
                                }.font(.caption)
                                Spacer()
                                if intent.state == "prepared" {
                                    Button("继续原发送") { Task { await store.continueSendIntent(intent) } }
                                        .disabled(!ready(intent))
                                } else {
                                    Button("查询结果") { Task { await store.querySendIntent(intent) } }
                                        .disabled(!store.isServiceRunning || store.sending)
                                    Button("新发送") { newSend = intent }.disabled(!ready(intent))
                                }
                            }.font(.caption)
                        }
                    }
                }.frame(maxHeight: 92)
            }
            .padding(9)
            .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
            .alert("作为新短信发送？", isPresented: Binding(get: { newSend != nil }, set: { if !$0 { newSend = nil } })) {
                Button("取消", role: .cancel) { newSend = nil }
                Button("新发送") {
                    if let intent = newSend {
                        Task { await store.send(phone: intent.phone, body: intent.body, expectedProfileID: intent.profileID, newIntent: true) }
                    }
                    newSend = nil
                }
            } message: {
                Text("上次请求可能已经提交。新发送会生成独立请求，可能产生重复短信；仍使用原号码与内容。")
            }
        }
    }
}
