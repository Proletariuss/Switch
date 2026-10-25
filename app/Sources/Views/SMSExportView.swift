import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct SMSExportView: View {
    var profiles: [SMSExportProfileOption] = []
    var subscriptions: [SMSExportSubscriptionOption] = []
    @Environment(\.dismiss) private var dismiss
    @State private var source = "all"
    @State private var device = ""
    @State private var profile = ""
    @State private var subscription = ""
    @State private var peer = ""
    @State private var privacy = "masked"
    @State private var usesDateRange = false
    @State private var from = Calendar.current.date(byAdding: .day, value: -30, to: Date()) ?? Date()
    @State private var to = Date()
    @State private var task: Task<Void, Never>?
    @State private var choosingFile = false
    @State private var progress: Double = 0
    @State private var transferring = false
    @State private var feedback: String?
    @State private var failed = false
    @State private var exporter = SMSArchiveExporter()

    private var busy: Bool { task != nil || choosingFile }
    private var availableSubscriptions: [SMSExportSubscriptionOption] {
        var seen = Set<String>()
        return subscriptions.filter { device.isEmpty || $0.device == device }
            .filter { seen.insert($0.id).inserted }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Label("导出短信归档", systemImage: "square.and.arrow.up").font(.title2.bold())
                Spacer()
                Button(busy ? "取消导出" : "关闭") {
                    if let task { task.cancel() } else if !choosingFile { dismiss() }
                }.keyboardShortcut(.cancelAction).disabled(choosingFile)
            }
            Form {
                Section("导出范围") {
                    Picker("短信来源", selection: $source) {
                        Text("全部来源").tag("all")
                        Text("设备短信").tag("device")
                        Text("N待短信").tag("nwait")
                    }
                    if source == "device" {
                        Picker("设备线路", selection: $device) {
                            Text("所有设备线路").tag("")
                            Text("模块").tag("dji")
                            Text("读卡器").tag("reader")
                        }
                        if !availableSubscriptions.isEmpty {
                            Picker("号卡身份", selection: $subscription) {
                                Text("全部身份（含未知历史）").tag("")
                                ForEach(availableSubscriptions) { Text($0.title).tag($0.id) }
                            }
                        }
                    }
                    if source == "nwait" {
                        if profiles.isEmpty {
                            TextField("Profile ID（留空为全部）", text: $profile)
                        } else {
                            Picker("Profile", selection: $profile) {
                                Text("全部 Profile").tag("")
                                ForEach(profiles) { Text($0.title).tag($0.id) }
                            }
                        }
                    }
                    TextField("对方号码（精确匹配，留空为全部）", text: $peer)
                    Toggle("限定时间范围", isOn: $usesDateRange)
                    if usesDateRange {
                        DatePicker("从", selection: $from, displayedComponents: [.date, .hourAndMinute])
                        DatePicker("至", selection: $to, displayedComponents: [.date, .hourAndMinute])
                        if from > to { Text("开始时间不能晚于结束时间。").foregroundStyle(.orange) }
                    }
                }
                Section("文件内容") {
                    Picker("隐私遮盖", selection: $privacy) {
                        Text("隐藏号码、正文与号卡标识").tag("masked")
                        Text("导出完整内容").tag("full")
                    }
                    Text(privacy == "full" ? "完整 JSON 包含短信正文、号码与线路标识，保存后可离线查看。" : "保留来源、方向、状态与时间；号码、正文和号卡标识不写入文件。")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("导出本机已归档短信。设备与 N待分别取各自的一致时间截面，文件中记录截取时间；不会扫描号卡，也不会改变已读或转发进度。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
            .disabled(busy)
            if task != nil {
                if transferring { ProgressView(value: progress) }
                else { ProgressView("正在准备归档…").controlSize(.small) }
            }
            if let feedback {
                Text(feedback).font(.callout).foregroundStyle(failed ? Color.orange : Color.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Text("JSON · 分块保存").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("选择位置并导出…", action: chooseDestination)
                    .buttonStyle(.borderedProminent).disabled(busy || (usesDateRange && from > to))
            }
        }
        .padding(22)
        .frame(width: 570, height: 610)
        .background(WorkspaceColors.canvas)
        .interactiveDismissDisabled(busy)
        .onChange(of: device) { subscription = "" }
        .onDisappear { task?.cancel() }
    }
    private func chooseDestination() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "Switch-短信归档-\(Date().formatted(.iso8601.year().month().day().dateSeparator(.dash))).json"
        panel.canCreateDirectories = true
        choosingFile = true
        panel.begin { response in
            choosingFile = false
            guard response == .OK, let url = panel.url else { return }
            start(url)
        }
    }
    private func start(_ destination: URL) {
        let trimmedPeer = peer.trimmingCharacters(in: .whitespacesAndNewlines)
        let selectedProfile = profile.trimmingCharacters(in: .whitespacesAndNewlines)
        var request = SMSArchiveExportRequest(source: source, privacy: privacy)
        request.device = source == "device" && !device.isEmpty ? device : nil
        request.subscriptionID = source == "device" && !subscription.isEmpty ? subscription : nil
        request.profileID = source == "nwait" && !selectedProfile.isEmpty ? selectedProfile : nil
        request.peer = trimmedPeer.isEmpty ? nil : trimmedPeer
        if usesDateRange {
            request.fromMS = UInt64(max(0, from.timeIntervalSince1970 * 1000))
            request.toMS = UInt64(max(0, to.timeIntervalSince1970 * 1000))
        }
        feedback = nil; failed = false; progress = 0; transferring = false
        task = Task { @MainActor in
            defer { task = nil }
            do {
                let result = try await exporter.save(request, to: destination) { done, total in
                    transferring = true
                    progress = total > 0 ? Double(done) / Double(total) : 0
                }
                feedback = "已导出 \(result.messageCount) 条短信。"
            } catch is CancellationError {
                feedback = "已取消导出，目标文件未替换。"
            } catch {
                failed = true
                feedback = error.localizedDescription
            }
        }
    }
}
