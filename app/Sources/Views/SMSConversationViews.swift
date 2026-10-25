import SwiftUI

/// 会话：按号码分组的短信（消息按时间正序，最新在底部）
struct Conversation: Identifiable {
    let phone: String
    var messages: [SMSItem]
    let lineID: String

    init(phone: String, messages: [SMSItem], lineID: String = "legacy") {
        self.phone = phone
        self.messages = messages
        self.lineID = lineID
    }

    var subscriptionID: String? { messages.first?.subscription }
    var id: String { SMSConversationIdentity.device(lineID: lineID, peer: phone, subscriptionID: subscriptionID) }
    var lastMessage: SMSItem? { messages.last }
    var lastTimestamp: Date { messages.last?.timestamp ?? .distantPast }
    var senderForDeletion: String? {
        messages.compactMap(\.sender).first { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }
}

struct ConversationRow: View {
    @Environment(\.workspaceViewport) private var viewport
    let conversation: Conversation
    @EnvironmentObject private var dashboardStore: DashboardStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            WorkspaceSymbol(systemName: "bubble.left.fill", tint: ProletariusPalette.action, size: 26)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 3) {
                Text(PrivacyMask.value(conversation.phone, enabled: privacyMaskEnabled))
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
					.copyableInformation(conversation.phone, enabled: !privacyMaskEnabled)
                if let last = conversation.lastMessage {
                    HStack(spacing: 6) {
                        if last.isOutgoing {
                            Text("我：")
                                .foregroundStyle(.secondary)
                        }
                        Text(privacyMaskEnabled ? "内容已隐藏" : (last.content ?? ""))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .font(.caption)
                }
            }
            Spacer()
            if let last = conversation.lastMessage {
                Text(last.timestamp.formatted(date: .omitted, time: .shortened))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, viewport.isCompactHeight ? 6 : 9)
    }
}

/// 聊天界面：气泡列表 + 输入框
struct ConversationDetailView: View {
    @Environment(\.workspaceViewport) private var viewport
    @EnvironmentObject private var smsStore: SMSStore
	@EnvironmentObject private var dashboardStore: DashboardStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let conversation: Conversation
    var focusMessageID: String? = nil
    let onOpenMessage: (SMSItem) -> Void
    let onDeleteMessage: (SMSItem) -> Void
    let onDeleteConversation: (String, Int) -> Void
    let onSent: () -> Void

    @State private var draft = ""
    @State private var sending = false
    @State private var sendError: String?
    @State private var selectedLineID = ""
    @State private var didFocusMessage = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "bubble.left.and.bubble.right.fill", tint: ProletariusPalette.action, size: 28)
                VStack(alignment: .leading, spacing: 4) {
                    Text(PrivacyMask.value(conversation.phone, enabled: privacyMaskEnabled))
                        .font(.headline)
                        .copyableInformation(conversation.phone, enabled: !privacyMaskEnabled)
                    Text("\(conversation.messages.count) 条短信")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding(viewport.cardPadding)

            Divider()

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(spacing: viewport.isCompactHeight ? 10 : 14) {
                        ForEach(conversation.messages) { item in
                            MessageBubble(
                                item: item,
                                conversationSender: conversation.senderForDeletion,
                                onOpen: { onOpenMessage(item) },
                                onDelete: { onDeleteMessage(item) },
                                onDeleteConversation: { sender in
                                    onDeleteConversation(sender, conversation.messages.count)
                                })
                            .id(item.id)
                        }
                    }
                    .padding(viewport.cardPadding)
                    .frame(maxWidth: .infinity)
                }
                .scrollIndicators(.visible)
                .scrollBounceBehavior(.basedOnSize)
                .onAppear {
                    if let focusMessageID, conversation.messages.contains(where: { $0.id == focusMessageID }) {
                        proxy.scrollTo(focusMessageID, anchor: .center)
                        didFocusMessage = true
                        return
                    }
                    if let last = conversation.messages.last {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
                .compatibleOnChange(of: conversation.messages.count) { _ in
                    if !didFocusMessage, let focusMessageID, conversation.messages.contains(where: { $0.id == focusMessageID }) {
                        proxy.scrollTo(focusMessageID, anchor: .center)
                        didFocusMessage = true
                        return
                    }
                    if let last = conversation.messages.last {
                        withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                }
                .compatibleOnChange(of: focusMessageID) { id in
                    if let id, conversation.messages.contains(where: { $0.id == id }) {
                        proxy.scrollTo(id, anchor: .center)
                        didFocusMessage = true
                    }
                }
            }
            .background(WorkspaceColors.canvas)

            Divider()

            if let sendError {
                Text(sendError)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 14)
                    .padding(.top, 8)
            }

            VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 8 : 12) {
                HStack(spacing: 8) {
                    Text("发送线路")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Picker("发送设备", selection: $selectedLineID) {
                        if !smsStore.readySendDevices.contains(where: { $0.selectionID == selectedLineID }) {
                            Text("请选择发送线路").tag("")
                        }
                        ForEach(smsStore.readySendDevices) { device in
                            Text(device.displayName(privacyMaskEnabled: privacyMaskEnabled)).tag(device.selectionID)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .disabled(sending || smsStore.readySendDevices.isEmpty)
                }
                HStack(spacing: 10) {
                    TextField("短信内容", text: $draft)
                        .textFieldStyle(.workspace)
                        .onSubmit { send() }
                    Button {
                        send()
                    } label: {
                        if sending {
                            ProgressView().controlSize(.small)
                        } else {
                            HStack(spacing: 6) {
                                TelegramPlaneIcon(size: 18)
                                Text("发送")
                            }
                        }
                    }
                    .buttonStyle(.proletariusPrimary)
                    .disabled(sending
                        || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || !smsStore.readySendDevices.contains(where: { $0.selectionID == selectedLineID }))
                }
            }
            .padding(viewport.cardPadding)
        }
        .onAppear { selectOriginalSubscription() }
        .compatibleOnChange(of: smsStore.readySendDevices.map(\.selectionID)) { ids in
            if !ids.contains(selectedLineID) { selectedLineID = "" }
        }
        .compatibleOnChange(of: conversation.id) { _ in
            selectOriginalSubscription()
            draft = ""
            sendError = nil
        }
    }

    private func selectOriginalSubscription() {
        selectedLineID = SMSDeviceStatus.replySelection(lineID: conversation.lineID,
            subscriptionID: conversation.subscriptionID, devices: smsStore.readySendDevices)
    }

    private func send() {
        let message = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty, !sending else { return }
        guard let line = smsStore.readySendDevices.first(where: { $0.selectionID == selectedLineID }) else {
            sendError = "原线路暂不可用，请明确选择发送线路。"
            return
        }
        let lineID = line.id
        let threadID = conversation.id
        sending = true
        sendError = nil
        Task {
            do {
                _ = try await DeviceSMSSendStore.shared.send(phone: conversation.phone,
                    message: message, device: lineID, subscriptionID: line.subscription)
                await MainActor.run {
                    sending = false
                    if conversation.id == threadID && draft.trimmingCharacters(in: .whitespacesAndNewlines) == message { draft = "" }
                    onSent()
                }
            } catch {
                await MainActor.run {
                    sending = false
                    sendError = smsSendErrorMessage(error)
                    if operationOutcomeIsUnknown(error) { smsStore.refresh() }
                }
            }
        }
    }
}

/// 聊天气泡
struct MessageBubble: View {
    let item: SMSItem
    let conversationSender: String?
    let onOpen: () -> Void
    let onDelete: () -> Void
    let onDeleteConversation: (String) -> Void

    private var isOutgoing: Bool { item.isOutgoing }

    var body: some View {
        HStack {
            if isOutgoing {
                Spacer(minLength: 60)
            }
            VStack(alignment: isOutgoing ? .trailing : .leading, spacing: isOutgoing ? 2 : 3) {
                Text(item.content ?? "")
                    .font(isOutgoing ? .system(size: 14) : .callout)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                HStack(spacing: isOutgoing ? 4 : 6) {
                    if let code = item.code, !code.isEmpty {
                        Text("验证码 \(code)")
                            .font(.caption2.bold())
                            .padding(.horizontal, 5).padding(.vertical, 1)
                            .background(Capsule().fill(Color.orange.opacity(0.25)))
                            .foregroundStyle(.orange)
                    }
                    if let deviceLabel = deviceLabel {
                        Text(deviceLabel)
                            .font(.caption2)
                            .opacity(0.7)
                    }
                    Text(item.timestamp.formatted(date: .omitted, time: .shortened))
                        .font(.caption2)
                        .opacity(0.7)
                }
                .lineLimit(1)
            }
            .padding(.horizontal, isOutgoing ? 11 : 14)
            .padding(.vertical, isOutgoing ? 5 : 9)
            .background(
                RoundedRectangle(cornerRadius: isOutgoing ? 10 : 12, style: .continuous)
                    .fill(isOutgoing ? WorkspaceColors.accent : WorkspaceColors.surface))
            .foregroundStyle(isOutgoing ? .white : .primary)
            .overlay(
                RoundedRectangle(cornerRadius: isOutgoing ? 10 : 12, style: .continuous)
                    .stroke(isOutgoing ? Color.clear : WorkspaceColors.outline, lineWidth: 1))
            .contentShape(Rectangle())
            .onTapGesture { onOpen() }
            .help("查看详情")
            if !isOutgoing {
                Spacer(minLength: 60)
            }
        }
        .frame(maxWidth: 560)
        .frame(maxWidth: .infinity)
        .contextMenu {
            Button("查看详情") {
                onOpen()
            }

            Divider()

            Button("删除此短信…", role: .destructive) {
                onDelete()
            }

            if let conversationSender {
                Button("删除该号码的全部短信…", role: .destructive) {
                    onDeleteConversation(conversationSender)
                }
            }
        }
    }

    private var deviceLabel: String? {
        guard let value = item.device?.lowercased(), !value.isEmpty else { return nil }
        switch CommunicationDeviceID(rawValue: value) {
        case .reader: return "读卡器"
        case .dji: return "DJI"
        case nil: return value
        }
    }
}

/// 短信详情弹窗：查看内容 + 回复 + 逐条删除
struct SMSDetailView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var smsStore: SMSStore
    @EnvironmentObject private var dashboardStore: DashboardStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    let item: SMSItem
    let onDelete: (Bool) -> Void
    let onReply: () -> Void

    @State private var deleting = false
    @State private var errorMessage: String?
    @State private var showingDeleteConfirm = false
    @State private var showReply = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "text.bubble.fill", tint: WorkspaceColors.accent, size: 34)
                WorkspaceSectionHeader(title: "短信详情", detail: "内容与存储信息")
                Spacer()
                Button("关闭") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }

            HStack(spacing: 10) {
                Text("发送者").foregroundStyle(.secondary).frame(width: 60, alignment: .leading)
                Text(PrivacyMask.value(item.sender ?? "未知号码", enabled: privacyMaskEnabled))
					.copyableInformation(item.sender ?? "", enabled: !privacyMaskEnabled)
                Spacer()
                if let code = item.code, !code.isEmpty {
                    Text("验证码 \(code)")
                        .font(.caption.bold())
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .background(Capsule().fill(Color.orange.opacity(0.2)))
                        .foregroundStyle(.orange)
                }
            }
            .font(.callout)

            HStack(spacing: 10) {
                Text("时间").foregroundStyle(.secondary).frame(width: 60, alignment: .leading)
                Text(item.timestamp.formatted(date: .long, time: .standard)).textSelection(.enabled)
                Spacer()
            }
            .font(.callout)

            if item.isFromModule {
                HStack(spacing: 10) {
                    Text("存储位置").foregroundStyle(.secondary).frame(width: 60, alignment: .leading)
                    Text(item.moduleStorage == "SM" ? "SIM 卡（SM）" : "模块存储（ME）")
                        .font(.callout)
                    Text("索引 \(item.moduleIndex ?? 0)")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                }
            } else if item.isArchived {
                HStack(spacing: 10) {
                    Text("存储位置").foregroundStyle(.secondary).frame(width: 60, alignment: .leading)
                    Text("本机存储")
                        .font(.callout)
                    Text(item.moduleStorage == "SM" ? "（原来自 SIM 卡）" : item.moduleStorage == "ME" ? "（原来自模块存储）" : "")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                }
            }

            Divider()
            Text("内容").foregroundStyle(.secondary).font(.caption)
            ScrollView {
                Text(item.content ?? "")
                    .font(.body)
                    .frame(maxWidth: .infinity, alignment: .leading)
					.copyableInformation(item.content ?? "")
            }
            .frame(maxHeight: .infinity)
            .padding(14)
            .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))

            if let errorMessage {
                Text(errorMessage).font(.caption).foregroundStyle(.red)
            }

            HStack {
                Button {
                    showReply = true
                } label: {
                    Label("回复", systemImage: "arrowshape.turn.up.left")
                }
                .disabled(deleting)
                Spacer()
                Button("删除此短信", role: .destructive) {
                    showingDeleteConfirm = true
                }
                .buttonStyle(.proletariusPrimary)
                .disabled(deleting)
            }
        }
        .padding(24)
        .frame(width: 520, height: 460)
        .background(WorkspaceColors.surface)
        .buttonStyle(.proletarius)
        .sheet(isPresented: $showReply) {
            ComposeSMSView(initialPhone: item.sender ?? "", initialLineID: item.lineID, onSent: {
                showReply = false
                onReply()
            })
        }
        .confirmationDialog("删除此短信？", isPresented: $showingDeleteConfirm, titleVisibility: .visible) {
            Button("删除", role: .destructive) { delete() }
            Button("取消", role: .cancel) {}
        } message: {
            Text(deleteMessage)
        }
    }

    private var deleteMessage: String {
        if item.isFromModule {
            return "将从\(item.moduleStorage == "SM" ? "SIM 卡" : "模块存储")中删除，不可恢复。"
        }
        if item.isArchived {
            return "将从本机存储中删除，不可恢复。"
        }
        return "将删除本机缓存中的这条记录。"
    }

    private func delete() {
        deleting = true
        Task {
            do {
                let deleted = try await smsStore.deleteMessage(item)
                deleting = false
                onDelete(deleted)
            } catch {
                deleting = false
                errorMessage = "删除失败：\(error.localizedDescription)"
            }
        }
    }
}

/// 清空短信存储：SIM 卡 / 模块 / 本机 可多选
struct SMSClearView: View {
    @Environment(\.dismiss) private var dismiss
    let readerMode: Bool
    let onDone: () -> Void

    @State private var clearSM = false
    @State private var clearME = false
    @State private var clearLocal: Bool
    @State private var clearing = false
    @State private var errorMessage: String?

    private var nothingSelected: Bool {
        !clearSM && !clearME && !clearLocal
    }

    init(readerMode: Bool, onDone: @escaping () -> Void) {
        self.readerMode = readerMode
        self.onDone = onDone
        _clearLocal = State(initialValue: readerMode)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "tray.fill", tint: WorkspaceColors.accent, size: 34)
                WorkspaceSectionHeader(title: "清空短信存储", detail: "存储位置")
            }
            Text("选择要清空的存储位置，可多选。删除后不可恢复。")
                .font(.caption).foregroundStyle(.secondary)
            Text("本机清理只删除短信历史；发送确认记录会保留，用于防止原请求重复提交。")
                .font(.caption).foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 12) {
                if !readerMode {
                    Toggle("SIM 卡存储（SM）", isOn: $clearSM)
                    Toggle("模块存储（ME）", isOn: $clearME)
                }
                Toggle("本机存储（已保存的短信）", isOn: $clearLocal)
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))

            if let errorMessage {
                Text(errorMessage).font(.callout).foregroundStyle(.red)
            }

            HStack {
                Button("取消") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button {
                    clear()
                } label: {
                    if clearing {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("清空")
                    }
                }
                .buttonStyle(.proletariusPrimary)
                .keyboardShortcut(.defaultAction)
                .disabled(clearing || nothingSelected)
            }
        }
        .padding(24)
        .frame(width: 440)
        .background(WorkspaceColors.surface)
        .buttonStyle(.proletarius)
    }

    private func clear() {
        clearing = true
        Task {
            do {
                let client = APIClient()
                let _: SMSClearResult = try await client.send(
                    "api/sms/clear",
                    body: SMSClearRequest(sm: clearSM, me: clearME, local: clearLocal))
                clearing = false
                onDone()
                dismiss()
            } catch {
                clearing = false
                errorMessage = "清空失败：\(error.localizedDescription)"
            }
        }
    }
}

/// 发送短信
struct ComposeSMSView: View {
    @EnvironmentObject private var smsStore: SMSStore
    @Environment(\.dismiss) private var dismiss
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    let onSent: () -> Void

    @State private var phone: String
    @State private var message = ""
    @State private var sending = false
    @State private var resultMessage: String?
    @State private var selectedLineID: String
    private let initialLineID: String?

    init(initialPhone: String = "", initialLineID: String? = nil, onSent: @escaping () -> Void) {
        self.onSent = onSent
        self.initialLineID = initialLineID
        _phone = State(initialValue: initialPhone)
        _selectedLineID = State(initialValue: "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 12) {
                TelegramPlaneIcon(size: 40)
                WorkspaceSectionHeader(title: "发送短信", detail: "选择线路与收件人")
            }
            Text("发送设备")
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
            Picker("发送设备", selection: $selectedLineID) {
                if !smsStore.readySendDevices.contains(where: { $0.selectionID == selectedLineID }) {
                    Text("请选择发送线路").tag("")
                }
                ForEach(smsStore.readySendDevices) { device in
                    Text(device.displayName(privacyMaskEnabled: privacyMaskEnabled)).tag(device.selectionID)
                }
            }
            .pickerStyle(.segmented)
            .disabled(sending || smsStore.readySendDevices.isEmpty)
            if smsStore.readySendDevices.isEmpty {
                Text("当前没有可用的短信发送设备")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            TextField("收件人号码（国际号码如 +86138XXXXXXXX）", text: $phone)
                .textFieldStyle(.workspace)
            TextEditor(text: $message)
                .font(.body)
                .frame(minHeight: 120)
                .scrollContentBackground(.hidden)
                .padding(10)
                .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 12))
                .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(WorkspaceColors.outline, lineWidth: 0.8))
                .compatibleOnChange(of: message) { newValue in
                    if newValue.count > 300 {
                        message = String(newValue.prefix(300))
                    }
                }
            Text("\(message.count)/300")
                .font(.caption).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .trailing)

            if let resultMessage {
                Text(resultMessage)
                    .font(.callout)
                    .foregroundStyle(resultMessage.contains("成功") ? .green : .red)
            }

            HStack {
                Button("取消") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button {
                    send()
                } label: {
                    if sending {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("发送")
                    }
                }
                .buttonStyle(.proletariusPrimary)
                .keyboardShortcut(.defaultAction)
                .disabled(sending
                    || phone.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    || message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    || !smsStore.readySendDevices.contains(where: { $0.selectionID == selectedLineID }))
            }
        }
        .padding(24)
        .frame(width: 520)
        .compatibleOnChange(of: smsStore.readySendDevices.map(\.selectionID)) { ids in
            if !ids.contains(selectedLineID) { selectedLineID = "" }
        }
        .background(WorkspaceColors.surface)
        .buttonStyle(.proletarius)

    }

    private func send() {
        let trimmedPhone = phone.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedMessage = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedPhone.isEmpty, !trimmedMessage.isEmpty, !sending else { return }
        guard let line = smsStore.readySendDevices.first(where: { $0.selectionID == selectedLineID }) else {
            resultMessage = "原线路暂不可用，请明确选择发送线路。"
            return
        }
        let lineID = line.id
        sending = true
        resultMessage = nil
        Task {
            do {
                let result = try await DeviceSMSSendStore.shared.send(phone: trimmedPhone,
                    message: trimmedMessage, device: lineID, subscriptionID: line.subscription)
                sending = false
                resultMessage = result.statusText
                onSent()
                dismiss()
            } catch {
                await MainActor.run {
                    sending = false
                    resultMessage = smsSendErrorMessage(error)
                    if operationOutcomeIsUnknown(error) { smsStore.refresh() }
                }
            }
        }
    }
}
