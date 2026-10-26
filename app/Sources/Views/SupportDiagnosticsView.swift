import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SupportDiagnosticsView: View {
    @EnvironmentObject private var backend: BackendProcess
    @Environment(\.workspaceViewport) private var viewport
    @State private var exporting = false
    @State private var feedback: String?
    @State private var request: Task<Void, Never>?
    @State private var exportRevision: UInt64 = 0

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "doc.text.magnifyingglass", tint: WorkspaceColors.accent, size: 40)
                WorkspaceSectionHeader(title: "运行日志与导出", detail: "默认持续记录，重启后保留最近日志。")
                Spacer()
                Button { export() } label: {
                    Label(exporting ? "正在导出" : "导出诊断包", systemImage: "square.and.arrow.up")
                }.disabled(exporting).controlSize(.small)
            }
            Text("记录本机请求、错误码、恢复阶段、后台启动与退出。请求关联号用于串联界面与后台记录。")
                .font(.callout).foregroundStyle(.secondary)
            Text("日志自动轮转：后台最多 8 MiB，客户端最多 3 MiB。导出包不包含短信正文、电话号码、号卡身份、Bot 密钥或代理密码。")
                .font(.caption).foregroundStyle(.secondary)
            Text("后台不可用时仍可导出客户端记录；未获取、已轮转或写入失败的部分会在诊断包中明确标注。旧版本没有记录的历史无法补回。")
                .font(.caption).foregroundStyle(.secondary)
            if let feedback { Text(feedback).font(.callout).foregroundStyle(.secondary).textSelection(.enabled) }
            if exporting { ProgressView().controlSize(.small) }
            Spacer()
        }
        .padding(viewport.cardPadding).workspaceCard()
        .onDisappear { exportRevision &+= 1; request?.cancel(); request = nil; exporting = false }
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
