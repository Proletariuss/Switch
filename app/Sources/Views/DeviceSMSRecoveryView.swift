import SwiftUI

/// The backend owns the durable request. Merely opening this view only reads it.
struct DeviceSMSRecoveryView: View {
    @ObservedObject private var store = DeviceSMSSendStore.shared
    @EnvironmentObject private var smsStore: SMSStore
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var masked = true
    @State private var expanded = false
    @State private var newSend: DeviceSMSIntent?

    var body: some View {
        if !store.intents.isEmpty || store.error != nil {
            DisclosureGroup(isExpanded: $expanded) {
                if let error = store.error { Text(error).font(.caption).foregroundStyle(.secondary) }
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        ForEach(store.intents) { intent in
                            HStack(spacing: 8) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(PrivacyMask.value(intent.phone, enabled: masked)).font(.caption.weight(.medium))
                                    Text(intent.statusText).font(.caption).foregroundStyle(.secondary)
                                    Text(SMSDeviceStatus.historyLabel(lineID: intent.device,
                                        subscriptionID: intent.subscriptionID, devices: smsStore.devices, masked: masked))
                                        .font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                                }
                                Spacer(minLength: 4)
                                Button { newSend = intent } label: {
                                    Image(systemName: "arrow.clockwise").frame(width: 16, height: 16)
                                }.help("重新发送").accessibilityLabel("重新发送")
                                Button { store.dismiss(intent) } label: {
                                    Image(systemName: "trash").frame(width: 16, height: 16)
                                }.help("移除记录").accessibilityLabel("移除记录")
                            }
                            .font(.caption)
                            .disabled(!store.available || store.sending)
                        }
                    }
                }.frame(maxHeight: 130)
            } label: {
                HStack {
                    Label("设备发送待确认 · \(store.intents.count)", systemImage: "clock.arrow.circlepath")
                        .font(.caption.weight(.medium))
                    Spacer()
                    Button { store.clearRecoveryRecords() } label: {
                        Image(systemName: "trash").frame(width: 16, height: 16)
                    }.help("清空待确认记录").accessibilityLabel("清空待确认记录")
                        .disabled(store.sending || store.intents.isEmpty)
                }
            }
            .padding(10)
            .workspaceCard()
            .sheet(item: $newSend) { DeviceSMSNewRequestView(intent: $0) }
        }
    }
}

private struct DeviceSMSNewRequestView: View {
    let intent: DeviceSMSIntent
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var smsStore: SMSStore
    @ObservedObject private var store = DeviceSMSSendStore.shared
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var masked = true
    @State private var selection = ""
    @State private var submitting = false
    @State private var error: String?

    private var selected: SMSDeviceStatus? { smsStore.readySendDevices.first { $0.selectionID == selection } }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("作为新短信发送").font(.headline)
            Text("原请求可能已经提交。新发送使用独立请求，可能产生重复短信；请核对并重新选择发送线路。")
                .font(.callout).foregroundStyle(.secondary)
            Text(PrivacyMask.value(intent.phone, enabled: masked)).font(.body.monospaced())
            Text(masked ? "短信内容已隐藏" : intent.message).lineLimit(5).textSelection(.enabled)
            Picker("发送线路", selection: $selection) {
                Text("请选择发送线路").tag("")
                ForEach(smsStore.readySendDevices) { device in
                    Text(device.displayName(privacyMaskEnabled: masked)).tag(device.selectionID)
                }
            }
            .disabled(submitting)
            if let error { Text(error).font(.caption).foregroundStyle(.red) }
            HStack {
                Button("取消") { dismiss() }.disabled(submitting)
                Spacer()
                Button("确认新发送") { send() }
                    .buttonStyle(.proletariusPrimary)
                    .disabled(selected == nil || submitting || store.sending || !store.available)
            }
        }
        .padding(24).frame(width: 480)
        .background(WorkspaceColors.surface)
        .buttonStyle(.proletarius)
        .interactiveDismissDisabled(submitting)
        .compatibleOnChange(of: smsStore.readySendDevices.map(\.selectionID)) { ids in
            if !ids.contains(selection) { selection = "" }
        }
    }

    private func send() {
        guard let selected, !submitting else { return }
        submitting = true
        error = nil
        Task {
            do {
                _ = try await store.send(phone: intent.phone, message: intent.message,
                    device: selected.id, subscriptionID: selected.subscription, newRequest: true)
                smsStore.refresh(force: true)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
            submitting = false
        }
    }
}
