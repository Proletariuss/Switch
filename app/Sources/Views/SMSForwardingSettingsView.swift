import SwiftUI

struct SMSForwardingSettingsView: View {
    @ObservedObject var store: SMSForwardingStore
    @Environment(\.dismiss) private var dismiss
    @State private var pendingAction: QueueAction?
    @State private var pendingNewTestProvider: String?
    @State private var confirmingInitialSetupCompletion = false
    private struct QueueAction: Identifiable {
        let id: String
        let provider: String
        let action: String
        let configurationID: String
        let attempts: Int
        let state: String
        let revision: String
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("启用后，新收到的设备短信与 N待短信会进入加密本地队列，再转发到已启用的服务。历史短信不会补发；短信发送记录不会转发。")
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    if let error = store.errorMessage, !error.isEmpty {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                            .textSelection(.enabled)
                    }
                    if let status = store.status {
                        Label("队列中 \(status.pending) 条 · Telegram \(status.telegramEnabled ? "开启" : "关闭") · Bark \(status.barkEnabled ? "开启" : "关闭")", systemImage: "checkmark.circle.fill")
                            .foregroundStyle(.secondary)
                        if let lastError = status.lastError {
                            Label(deliveryStatusText(lastError), systemImage: "arrow.clockwise.circle")
                                .foregroundStyle(.orange)
                        }
                    }
                }

                if store.needsInitialSetupRecovery {
                    Section("未完成的首次设置") {
                        Text("如首次设置意外中断，可检查并完成原设置。不会删除队列或生成替代密钥。")
                            .font(.caption).foregroundStyle(.secondary)
                        Text("仅支持首次设置留下的空队列。含旧版服务凭据的中断迁移无法在此完成，请使用原版本或人工恢复；原数据与凭据会保留。")
                            .font(.caption).foregroundStyle(.secondary)
                        if let message = store.credentialRecoveryMessage {
                            Text(message).font(.caption).foregroundStyle(.secondary)
                        }
                        HStack {
                            Button("检查未完成的设置") { Task { await store.inspectInitialSetupRecovery() } }
                            Button("完成首次设置") { confirmingInitialSetupCompletion = true }
                                .disabled(!store.canAttemptInitialSetupCompletion)
                            if store.isCredentialRecoveryInProgress { ProgressView().controlSize(.small) }
                        }
                        .disabled(store.isCredentialRecoveryInProgress || store.isSaving)
                    }
                }

                if let status = store.status {
                    Section("转发任务") {
                        if let telegram = status.telegram { channelSummary("Telegram", telegram) }
                        if let bark = status.bark { channelSummary("Bark", bark) }
                        Text("关闭渠道会暂停待发送任务。修改接收目标不会把旧任务改投到新目标；请恢复原目标，或取消旧任务。状态记录只保留任务编号和结果，不显示短信内容。")
                            .font(.caption).foregroundStyle(.secondary)
                        ForEach(status.tasks ?? []) { task in
                            DisclosureGroup("任务 \(task.id.prefix(8)) · \(task.updatedAt.formatted(date: .abbreviated, time: .shortened))") {
                                taskChannel(task, "telegram", "Telegram", task.telegram)
                                taskChannel(task, "bark", "Bark", task.bark)
                            }
                        }
                        if status.tasksTruncated == true {
                            Text("最多显示 100 项任务，优先显示待发送项。其他待发送任务仍保留在加密队列中。")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }

                Section {
                    Toggle("启用 Telegram Bot", isOn: $store.telegramEnabled)
                    LabeledContent("Bot Token") {
                        SecureField("从 BotFather 获取", text: $store.telegramBotToken)
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 330)
                    }
                    LabeledContent("Chat ID") {
                        TextField("个人或群组 Chat ID", text: $store.telegramChatID)
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 330)
                    }
                    Picker("代理网络", selection: $store.telegramProxyMode) {
                        Text("系统代理（默认）").tag("system")
                        Text("自定义代理").tag("manual")
                        Text("直连").tag("direct")
                    }
                    if store.telegramProxyMode == "manual" {
                        LabeledContent("代理地址") {
                            SecureField("socks5h://host:1080 或 https://host:port", text: $store.telegramProxyURL)
                                .textFieldStyle(.roundedBorder)
                                .frame(maxWidth: 330)
                        }
                        Text("支持 HTTP、HTTPS、SOCKS5 和代理认证。SOCKS5 域名由代理解析；已有的 socks5:// 地址会自动按此方式使用。地址保存在钥匙串中。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    HStack {
                        Text("Telegram Bot 不使用端到端加密。请先在 Telegram 中主动打开 Bot 并发送 /start，Bot 才能向你发送通知。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 8)
                        Button {
                            Task { await store.test("telegram") }
                        } label: {
                            if store.isTestingTelegram { ProgressView().controlSize(.small) }
                            else {
                                HStack(spacing: 6) {
                                    TelegramPlaneIcon(size: 16)
                                    Text(store.hasUnresolvedTest("telegram") ? "查询回执" : "测试")
                                }
                            }
                        }
                        .disabled((!store.telegramEnabled && !store.hasUnresolvedTest("telegram")) || store.isTestingTelegram || store.isSaving)
                        if store.canStartNewTestAfterUnknown("telegram") {
                            Button("重新测试…") { pendingNewTestProvider = "telegram" }
                                .disabled(!store.telegramEnabled || store.isTestingTelegram || store.isSaving)
                        }
                    }
                    if let result = store.telegramTestMessage {
                        Text(result).font(.caption).foregroundStyle(.secondary)
                    }
                } header: {
                    HStack(spacing: 6) {
                        TelegramPlaneIcon(size: 16)
                        Text("Telegram")
                    }
                }

                Section {
                    Toggle("启用 Bark", isOn: $store.barkEnabled)
                    LabeledContent("服务器") {
                        TextField("https://api.day.app", text: $store.barkServerURL)
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 330)
                    }
                    LabeledContent("设备密钥") {
                        SecureField("Bark 设备密钥", text: $store.barkDeviceKey)
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 330)
                    }
                    LabeledContent("图标 URL") {
                        TextField("可选，HTTPS 图片地址", text: $store.barkIconURL)
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 330)
                    }
                    LabeledContent("通知分组") {
                        TextField("Switch-SMS", text: $store.barkGroup)
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 330)
                    }
                    Toggle("加密通知内容", isOn: $store.barkEncrypted)
                    if store.barkEncrypted {
                        Picker("加密算法", selection: $store.barkEncryptionAlgorithm) {
                            Text("AES-128-CBC").tag("AES-128-CBC")
                            Text("AES-256-CBC").tag("AES-256-CBC")
                        }
                        LabeledContent("加密密钥") {
                            SecureField("AES-128 为 16 字节，AES-256 为 32 字节", text: $store.barkEncryptionKey)
                                .textFieldStyle(.roundedBorder)
                                .frame(maxWidth: 330)
                        }
                        Text("密钥按所选算法长度校验；每条通知都会使用新的随机 IV。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    HStack {
                        Text("远程 Bark 服务必须使用 HTTPS。设备密钥与加密密钥保存在钥匙串中。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 8)
                        Button {
                            Task { await store.test("bark") }
                        } label: {
                            if store.isTestingBark { ProgressView().controlSize(.small) }
                            else { Label(store.hasUnresolvedTest("bark") ? "查询回执" : "测试", systemImage: "bell.badge") }
                        }
                        .disabled((!store.barkEnabled && !store.hasUnresolvedTest("bark")) || store.isTestingBark || store.isSaving)
                        if store.canStartNewTestAfterUnknown("bark") {
                            Button("重新测试…") { pendingNewTestProvider = "bark" }
                                .disabled(!store.barkEnabled || store.isTestingBark || store.isSaving)
                        }
                    }
                    if let result = store.barkTestMessage {
                        Text(result).font(.caption).foregroundStyle(.secondary)
                    }
                } header: {
                    Label("Bark", systemImage: "bell.badge.fill")
                }
            }
            .formStyle(.grouped)
            .navigationTitle("短信转发")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("关闭") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        Task { await store.save() }
                    } label: {
                        if store.isSaving { ProgressView().controlSize(.small) }
                        else { Text("保存并应用") }
                    }
                    .disabled(store.isSaving || store.isCredentialRecoveryInProgress)
                    .keyboardShortcut(.defaultAction)
                }
            }
        }
        .frame(minWidth: 620, minHeight: 650)
        .task {
            while !Task.isCancelled {
                await store.refreshStatus()
                do { try await Task.sleep(nanoseconds: 3_000_000_000) }
                catch { return }
            }
        }
        .confirmationDialog("完成未完成的首次设置？",
            isPresented: $confirmingInitialSetupCompletion, titleVisibility: .visible) {
            Button("确认完成首次设置") { Task { await store.completeInterruptedInitialSetup() } }
            Button("取消", role: .cancel) { }
        } message: {
            Text("将重新核对唯一原配置与空队列，并完成本机保存。不会清空数据、更换密钥或发送测试通知。")
        }
        .alert(queueActionTitle,
               isPresented: Binding(get: { pendingAction != nil }, set: { if !$0 { pendingAction = nil } })) {
            Button("返回", role: .cancel) { pendingAction = nil }
            Button(queueActionButton,
                   role: pendingAction?.action == "cancel" ? .destructive : nil) {
                if let action = pendingAction {
                    Task { await store.resolveTask(
                        id: action.id, provider: action.provider, action: action.action,
                        expectedConfigurationID: action.configurationID,
                        expectedAttempts: action.attempts, expectedState: action.state,
                        expectedDestinationRevision: action.revision) }
                }
                pendingAction = nil
            }
        } message: {
            Text(queueActionMessage)
        }
        .confirmationDialog("发起一条新的测试通知？",
            isPresented: Binding(get: { pendingNewTestProvider != nil },
                                 set: { if !$0 { pendingNewTestProvider = nil } }),
            titleVisibility: .visible) {
            Button("确认重新测试") {
                if let provider = pendingNewTestProvider {
                    Task { await store.startNewTestAfterUnknown(provider) }
                }
                pendingNewTestProvider = nil
            }
            Button("取消", role: .cancel) { pendingNewTestProvider = nil }
        } message: {
            Text("原测试可能已被服务接收，只是结果无法确认。重新测试会产生新的请求，接收端可能收到重复通知；Switch 不会重发原请求。请先检查接收端。")
        }
    }

    private func channelSummary(_ name: String, _ summary: SMSForwardingChannelSummary) -> some View {
        Text("\(name)：待发送 \(summary.pending) · 重试 \(summary.retrying) · 暂停 \(summary.paused) · 最近服务接收 \(summary.delivered)")
            .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
    }

    private func taskChannel(_ task: SMSForwardingTaskStatus, _ provider: String,
                             _ name: String, _ channel: SMSForwardingChannelStatus) -> some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 3) {
                Text("\(name) · \(channel.label)")
                Text("已尝试 \(channel.attempts) 次").font(.caption).foregroundStyle(.secondary)
                if let total = channel.totalParts, total > 1 {
                    Text("服务已确认 \(channel.confirmedParts ?? 0)/\(total) 片").font(.caption).foregroundStyle(.secondary)
                }
                if let next = channel.nextAttemptAt, channel.state == "retry_wait" {
                    Text("下次尝试 \(next.formatted(date: .omitted, time: .standard))").font(.caption).foregroundStyle(.secondary)
                }
                if let code = channel.errorCode, !code.isEmpty {
                    Text(SMSForwardingFailurePresentation.message(code, provider: provider))
                        .font(.caption).foregroundStyle(.orange)
                }
            }
            Spacer(minLength: 8)
            if channel.canRetry {
                Button(channel.requiresRetryConfirmation ? "确认重试…" : "重试") {
                    if channel.requiresRetryConfirmation {
                        pendingAction = frozenAction(task.id, provider, "retry", channel)
                    } else {
                        guard let configurationID = store.status?.configurationID else { return }
                        Task { await store.resolveTask(
                            id: task.id, provider: provider, action: "retry",
                            expectedConfigurationID: configurationID,
                            expectedAttempts: channel.attempts, expectedState: channel.state,
                            expectedDestinationRevision: channel.revision) }
                    }
                }
            }
            if channel.state == "paused_destination_unverified" {
                Button("确认目标…") { pendingAction = frozenAction(task.id, provider, "bind_legacy", channel) }
            }
            if channel.canCancel {
                Button("取消…", role: .destructive) { pendingAction = frozenAction(task.id, provider, "cancel", channel) }
            }
        }
        .disabled(store.resolvingTaskID != nil || store.isSaving)
    }

    private func frozenAction(_ id: String, _ provider: String, _ action: String,
                              _ channel: SMSForwardingChannelStatus) -> QueueAction? {
        guard let configurationID = store.status?.configurationID else { return nil }
        return QueueAction(id: id, provider: provider, action: action,
                           configurationID: configurationID, attempts: channel.attempts,
                           state: channel.state, revision: channel.revision)
    }

    private var queueActionTitle: String {
        switch pendingAction?.action {
        case "cancel": return "取消此渠道的转发任务？"
        case "retry": return "重试接收结果未知的任务？"
        default: return "确认旧任务接收目标？"
        }
    }

    private var queueActionButton: String {
        switch pendingAction?.action {
        case "cancel": return "取消任务"
        case "retry": return "确认重试"
        default: return "绑定当前目标"
        }
    }

    private var queueActionMessage: String {
        switch pendingAction?.action {
        case "cancel": return "取消后不会再尝试发送该渠道的任务。已经被远程服务接收的通知无法撤回。"
        case "retry": return "上一次通知可能已被服务接收，但未获得有效确认。再次发送可能产生重复通知。仅在核对接收端后确认重试；任务仍发送到原接收目标。"
        default: return "旧版本任务没有记录接收目标。确认后，此任务将使用已保存的当前渠道目标发送。请先核对 Bot、Chat ID 或 Bark 设备密钥。"
        }
    }

    private func deliveryStatusText(_ code: String) -> String {
        switch code {
        case "pending_channels_paused": return "部分转发任务已暂停，请查看下方渠道状态。"
        case "provider_delivery_pending": return "通知暂未送达，将自动重试。"
        case "queue_persistence_failed": return "本地转发队列无法保存，请检查磁盘空间或权限。"
        default:
            if code.hasPrefix("telegram_") {
                return SMSForwardingFailurePresentation.message(code, provider: "telegram")
            }
            if code.hasPrefix("bark_") {
                return SMSForwardingFailurePresentation.message(code, provider: "bark")
            }
            return "转发暂不可用，请检查渠道状态并刷新。"
        }
    }
}
