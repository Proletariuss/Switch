import SwiftUI

struct ModuleESIMView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.workspaceViewport) private var viewport
    @AppStorage(AppPreferenceKeys.privacyMaskEnabled) private var privacyMaskEnabled = true
    @ObservedObject var store: ModuleESIMStore
    let activeICCID: String?
    let activePhoneNumber: String?
    var onChange: () -> Void

    @State private var showingDownload = false
    @State private var deleting: ESIMProfileContext?

    private var operationActive: Bool { store.operation?.blocksNewMutation == true }

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 14) {
                WorkspaceSymbol(systemName: "simcard.2", tint: WorkspaceColors.accent, size: viewport.isCompactHeight ? 36 : 46)
                VStack(alignment: .leading, spacing: 5) {
                    Text("模块 eSIM")
                        .font(.system(size: viewport.isCompactHeight ? 20 : 23, weight: .bold))
                    Text("管理 Profile，切换当前使用的卡片")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    store.load(refresh: true)
                } label: {
                    Label("刷新", systemImage: "arrow.clockwise")
                }
                .disabled(store.busy || operationActive)
                Button("完成") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .buttonStyle(.proletarius)

            if let error = store.error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .font(.callout)
            }
            if let message = store.message, store.refreshOutcome?.completed != false {
                Label(message, systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.callout)
            }
            if let operation = store.operation, operation.blocksNewMutation {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Text(operation.message ?? "eSIM 操作进行中")
                            .font(.callout.weight(.medium))
                        Spacer()
                        Text("\(operation.progress)%")
                            .font(.callout.monospacedDigit().weight(.semibold))
                            .foregroundStyle(WorkspaceColors.accent)
                    }
                    ProgressView(value: Double(operation.progress), total: 100)
                        .tint(WorkspaceColors.accent)
                }
                .padding(viewport.isCompactHeight ? 12 : 16)
                .workspaceCard()
            }

            if store.overview == nil && store.busy {
                ProgressView("正在读取模块 eSIM…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.overview == nil, let error = store.error {
                ContentUnavailableView("eSIM 信息暂不可读", systemImage: "simcard", description: Text(error))
            } else if store.overview?.cardType == "physical_sim" {
                ContentUnavailableView("未检测到可管理的 eUICC", systemImage: "simcard", description: Text("确认模块已启用 eSIM Profile，并在模块 VoWiFi 关闭后刷新。"))
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: viewport.isCompactHeight ? 8 : 12) {
                        if let message = store.overview?.message?.trimmingCharacters(in: .whitespacesAndNewlines), !message.isEmpty {
                            Label(message, systemImage: "info.circle")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        ForEach(store.profiles) { item in
                            profileRow(item)
                        }
                        if store.profiles.isEmpty {
                            if let readError = profileReadError {
                                ContentUnavailableView("eUICC 已识别，但 Profile 列表读取失败", systemImage: "exclamationmark.triangle", description: Text(readError))
                            } else {
                                ContentUnavailableView("暂无 Profile", systemImage: "simcard.2", description: Text("可以通过激活码下载新的 eSIM Profile。"))
                            }
                        }
                    }
                }
                Divider().overlay(WorkspaceColors.outline)
                HStack {
                    Text("\(store.profiles.count) 个 Profile")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button {
                        showingDownload = true
                    } label: {
                        Label("下载", systemImage: "arrow.down.to.line")
                    }
                    .buttonStyle(.proletariusPrimary)
                    .disabled(store.busy || operationActive || store.overview?.capabilities?.canDownload != true)
                }
            }
        }
        .padding(viewport.pageInset)
        .frame(width: min(680, max(520, viewport.width - 44)), height: min(590, max(360, viewport.height - 44)))
        .environment(\.workspaceAccent, WorkspaceColors.accent)
        .environment(\.workspaceVisualStyle, .nekoko)
        .proletariusPageBackground()
        .sheet(isPresented: $showingDownload) {
            downloadSheet
        }
        .confirmationDialog(
            "删除此 Profile？",
            isPresented: Binding(
                get: { deleting != nil },
                set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible
        ) {
            if let item = deleting {
                Button("删除 \(item.profile.displayName)", role: .destructive) {
                    store.deleteProfile(item)
                    deleting = nil
                }
            }
            Button("取消", role: .cancel) { deleting = nil }
        } message: {
            Text("删除后无法恢复；当前正在使用的 Profile 不能删除。")
        }
        .onAppear { store.load() }
        .onChange(of: store.operation?.state) { _, state in
            if state == "succeeded" || state == "warning" {
                onChange()
            }
        }
    }

    private func profileRow(_ item: ESIMProfileContext) -> some View {
        let phone = profilePhoneNumber(item.profile.iccid)
        return HStack(alignment: .center, spacing: 14) {
            WorkspaceSymbol(systemName: item.profile.isEnabled ? "simcard.fill" : "simcard", tint: WorkspaceColors.accent, size: viewport.isCompactHeight ? 34 : 40)
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 7) {
                    Text(item.profile.flagEmoji ?? phone.flatMap(phoneFlagEmoji) ?? "🌐")
                    Text(item.profile.displayName)
                        .font(.system(size: 15, weight: .semibold))
                        .lineLimit(1)
                        .copyableInformation(item.profile.displayName)
                    if item.profile.isEnabled {
                        Label("当前使用", systemImage: "checkmark.circle.fill")
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(.green)
                    }
                }
                HStack(spacing: 6) {
                    Text("号码")
                        .foregroundStyle(.secondary)
                    Text(phone.map { PrivacyMask.value($0, enabled: privacyMaskEnabled) } ?? "号码未记录")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(phone == nil ? Color.secondary : Color.primary)
                        .copyableInformation(phone ?? "", enabled: phone != nil && !privacyMaskEnabled)
                }
                HStack(spacing: 4) {
                    Text("ICCID")
                    Text(PrivacyMask.value(item.profile.iccid, enabled: privacyMaskEnabled))
                        .font(.caption2.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .copyableInformation(item.profile.iccid, enabled: !privacyMaskEnabled)
                }
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            if !item.profile.isEnabled {
                VStack(spacing: 6) {
                    Button("切换") { store.switchProfile(item) }
                        .buttonStyle(.proletariusPrimary)
                        .controlSize(.small)
                        .disabled(store.busy || operationActive || store.overview?.capabilities?.canSwitch != true)
                    Button("删除", role: .destructive) { deleting = item }
                        .buttonStyle(.proletarius)
                        .controlSize(.small)
                        .disabled(store.busy || operationActive || store.overview?.capabilities?.canDelete != true)
                }
            }
        }
        .padding(viewport.isCompactHeight ? 12 : 16)
        .workspaceCard()
    }

    private func profilePhoneNumber(_ iccid: String) -> String? {
        if let saved = store.savedPhoneNumber(for: iccid) { return saved }
        guard let activeICCID, normalizedICCID(activeICCID) == normalizedICCID(iccid) else { return nil }
        return usableSIMPhoneNumber(activePhoneNumber)
    }

    private var profileReadError: String? {
        guard let group = store.overview?.profiles?.first(where: { $0.readState == "error" }) else { return nil }
        let detail = group.readError?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return detail.isEmpty
            ? "eUICC 已识别，但读取 Profile 文件失败。请关闭模块 VoWiFi 后刷新；不要重复写卡操作。"
            : detail
    }

    private func normalizedICCID(_ value: String) -> String {
        value.filter(\.isNumber)
    }

    private var downloadSheet: some View {
        DownloadESIMContent(
            overview: store.overview,
            operation: store.operation,
            banner: store.error.map {
                ESIMBanner(tone: .error, message: "下载 Profile 失败", detail: $0)
            },
            isDownloading: store.busy || operationActive,
            startDownload: { store.download($0) },
            confirmDownload: { store.confirmDownload(operationID: $0, confirm: $1) },
            deviceLabel: "模块直写",
            deviceName: "DJI 模块")
    }
}
