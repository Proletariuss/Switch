import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SupportDiagnosticsView: View {
    @EnvironmentObject private var backend: BackendProcess
    @Environment(\.workspaceViewport) private var viewport
    @StateObject private var live: LiveDiagnosticStore
    @State private var visible = false
    @State private var followLatest = true
    @State private var warningsOnly = false
    @State private var detailsExpanded = false
    @State private var exporting = false
    @State private var feedback: String?
    @State private var request: Task<Void, Never>?
    @State private var exportRevision: UInt64 = 0

    @MainActor init(store: LiveDiagnosticStore? = nil) {
        _live = StateObject(wrappedValue: store ?? LiveDiagnosticStore())
    }

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { heading; Spacer(); actions }
                VStack(alignment: .leading, spacing: 10) { heading; actions }
            }
            Text("后台：\(backendState) · \(live.status)"
                 + (live.updatedAt.map { " · \($0.formatted(date: .omitted, time: .standard))" } ?? ""))
                .font(.caption).foregroundStyle(live.updatedAt == nil ? Color.secondary : WorkspaceColors.accent)
                .lineLimit(2)
            Text(live.recovery.map { $0.components(separatedBy: " · ").prefix(2).joined(separator: " · ") }
                 ?? "模块恢复状态：当前未知，尚未取得有效的本次后台观察。")
                .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                .textSelection(.enabled)
            Picker("显示记录", selection: $warningsOnly) {
                Text("仅警告与错误").tag(true)
                Text("全部记录").tag(false)
            }.pickerStyle(.segmented).frame(maxWidth: 300)
            if let feedback {
                Text(feedback).font(.caption).foregroundStyle(.secondary)
                    .lineLimit(2).textSelection(.enabled)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        DisclosureGroup(isExpanded: $detailsExpanded) {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(live.recovery ?? "当前没有可确认的后台恢复观察。")
                                    .textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                                Text("这是最近一次观察，不代表重新核验或完成历史恢复。")
                                ForEach(live.notices, id: \.self) { message in
                                    Text(message).foregroundStyle(.orange)
                                }
                                Text("合并客户端、后台、模块与安全操作事件；最多显示最近 500 项。隐藏此页后停止轮询；导出仍包含有界持久日志。")
                                Text("仅显示闭集技术字段，不含号码、短信正文、密钥或原始通信全文。实时记录不保证已落盘。")
                            }
                            .font(.caption).foregroundStyle(.secondary)
                            .padding(.vertical, 8)
                        } label: {
                            Text("恢复详情与保留范围" + (live.notices.isEmpty ? "" : " · \(live.notices.count) 项提示"))
                                .font(.caption.weight(.medium))
                        }
                        .accessibilityIdentifier("live-diagnostics-details")
                        if !followLatest {
                            Text("自动滚动已暂停，记录仍在更新；较早记录可能移出窗口。")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        if displayedRows.isEmpty {
                            Text(warningsOnly ? "当前筛选没有警告或错误，可切换到全部记录。" : "当前实时窗口还没有可显示的记录。")
                                .foregroundStyle(.secondary).padding(.vertical, 16)
                        }
                        ForEach(displayedRows) { row in
                            VStack(alignment: .leading, spacing: 5) {
                                HStack(alignment: .firstTextBaseline) {
                                    Text(row.source).font(.caption.weight(.semibold))
                                    Text(row.title).font(.system(size: 12, weight: .medium, design: .monospaced))
                                    Spacer(minLength: 8)
                                    Text(row.date.formatted(date: .omitted, time: .standard))
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Text(row.detail).font(.system(size: 11, design: .monospaced))
                                    .foregroundStyle(row.warning ? Color.orange : Color.secondary)
                            }
                            .textSelection(.enabled)
                            .padding(10).frame(maxWidth: .infinity, alignment: .leading)
                            .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 8))
                            .id(row.id)
                        }
                        Color.clear.frame(height: 1).id("live-log-bottom")
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
                .scrollIndicators(.visible)
                .accessibilityIdentifier("live-diagnostic-log-scroll")
                .compatibleOnChange(of: displayedRows.last?.id ?? "") { _ in
                    if followLatest { proxy.scrollTo("live-log-bottom", anchor: .bottom) }
                }
                .compatibleOnChange(of: followLatest) { follow in
                    if follow { proxy.scrollTo("live-log-bottom", anchor: .bottom) }
                }
            }
            .frame(minHeight: viewport.isCompactHeight ? 180 : 200, maxHeight: .infinity)
        }
        .padding(viewport.cardPadding).workspaceCard()
        .onAppear {
            visible = true
            live.setLifecycle(visible: true, running: backend.state == .running, instance: backend.childPID)
        }
        .onDisappear {
            visible = false
            live.setLifecycle(visible: false, running: backend.state == .running, instance: backend.childPID)
            exportRevision &+= 1; request?.cancel(); request = nil; exporting = false
        }
        .compatibleOnChange(of: backend.state) { state in
            live.setLifecycle(visible: visible, running: state == .running, instance: backend.childPID)
        }
        .compatibleOnChange(of: backend.childPID) { child in
            live.setLifecycle(visible: visible, running: backend.state == .running, instance: child)
        }
    }

    private var displayedRows: [LiveDiagnosticRow] {
        warningsOnly ? live.rows.filter(\.warning) : live.rows
    }

    private var backendState: String {
        switch backend.state {
        case .running: return "运行中"
        case .starting: return "后台启动或恢复核对中"
        case .stopping: return "退出中"
        case .stopped: return "已停止"
        case .failed: return "启动或运行失败"
        }
    }

    private var heading: some View {
        HStack(spacing: 12) {
            WorkspaceSymbol(systemName: "doc.text.magnifyingglass", tint: WorkspaceColors.accent, size: 40)
            WorkspaceSectionHeader(title: "运行日志", detail: "实时查看安全状态、请求阶段与操作事件。")
        }
    }

    private var actions: some View {
        HStack(spacing: 10) {
            Toggle("跟随最新", isOn: $followLatest).toggleStyle(.checkbox)
                .help("关闭后暂停自动滚动，后台记录仍持续更新。")
            Button { export() } label: {
                Label(exporting ? "正在导出" : "导出诊断包", systemImage: "square.and.arrow.up")
            }.disabled(exporting)
            if exporting { ProgressView().controlSize(.small) }
        }.controlSize(.small)
    }

    private func export() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        let date = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "-")
        panel.nameFieldStringValue = "Switch-diagnostics-" + date + ".json"
        guard panel.runModal() == .OK, let destination = panel.url else { return }
        guard let lease = SupportExportLease.acquire() else {
            feedback = "上一份诊断包仍在处理，请等待后台文件操作结束后重试。"
            return
        }
        exportRevision &+= 1
        let revision = exportRevision
        exporting = true
        feedback = nil
        let isRunning = backend.state == .running
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unknown"
        request = Task { @MainActor in
            defer { lease.release(); if revision == exportRevision { exporting = false; request = nil } }
            let runtime: Data?
            if isRunning { runtime = try? await APIClient(timeoutInterval: 6).getData("api/diagnostics/support") }
            else { runtime = nil }
            guard !Task.isCancelled, revision == exportRevision else { return }
            do {
                let fileData = try await Task.detached(priority: .utility) {
                    try SupportBundleBuilder.make(client: ClientDiagnostics.shared.snapshot(),
                        backend: runtime, appVersion: version, build: build)
                }.value
                guard !Task.isCancelled, revision == exportRevision else { return }
                try await Task.detached(priority: .utility) {
                    try fileData.write(to: destination, options: [.atomic])
                    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: destination.path)
                }.value
                guard !Task.isCancelled, revision == exportRevision else { return }
                feedback = "诊断包已导出，可提供给维护者分析。缺失或跳过的记录已在包内标注。"
            } catch {
                guard !Task.isCancelled, revision == exportRevision else { return }
                feedback = "导出未完成，请确认目标位置可写后重试。"
            }
        }
    }
}
