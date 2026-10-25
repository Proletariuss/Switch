import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct DiagnosticEventsView: View {
    @EnvironmentObject private var backend: BackendProcess
    @Environment(\.workspaceViewport) private var viewport
    @StateObject private var store = DiagnosticEventStore()
    @State private var operation: DiagnosticOperation?
    @State private var request: Task<Void, Never>?
    @State private var feedback: String?

    private var events: [DiagnosticOperationEvent] {
        (store.snapshot?.events ?? []).reversed().filter { operation == nil || $0.operation == operation }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) { heading; Spacer(); actions }
                VStack(alignment: .leading, spacing: 10) { heading; actions }
            }
            Text("仅保留当前后端进程最近 \(store.snapshot?.capacity ?? 256) 项事件，重启清空。用于排查阶段与耗时；短信结果以发送记录为准。")
                .font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Picker("操作类型", selection: $operation) {
                Text("所有操作").tag(DiagnosticOperation?.none)
                ForEach(DiagnosticOperation.allCases, id: \.rawValue) { item in
                    Text(item.label).tag(Optional(item))
                }
            }.frame(maxWidth: 260)
            if let error = store.error { Text(error).font(.caption).foregroundStyle(.orange) }
            if let feedback { Text(feedback).font(.caption).foregroundStyle(.secondary) }
            Divider()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    if events.isEmpty {
                        Text(store.loading ? "读取操作记录…" : "当前没有此类操作记录。")
                            .foregroundStyle(.secondary).padding(.vertical, 16)
                    }
                    ForEach(events) { event in row(event) }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(viewport.cardPadding).workspaceCard()
        .onAppear { refresh() }
        .onDisappear { cancel() }
        .compatibleOnChange(of: backend.state) { state in
            if state != .running { cancel(); feedback = "后端未运行，当前内容为上次读取结果。" }
            else { feedback = "后端已重新运行，请刷新当前进程记录；列表仍为上次读取结果。" }
        }
    }
    private var heading: some View {
        HStack(spacing: 12) {
            WorkspaceSymbol(systemName: "list.bullet.rectangle", tint: WorkspaceColors.accent, size: 40)
            WorkspaceSectionHeader(title: "安全操作记录", detail: "限定字段，不含号码、短信正文、密钥或原始日志。")
        }
    }
    private var actions: some View {
        HStack(spacing: 10) {
            Button { export() } label: { Label("导出", systemImage: "square.and.arrow.up") }
                .disabled(store.snapshot == nil)
            Button { refresh() } label: { Label("刷新", systemImage: "arrow.clockwise") }
                .disabled(store.loading || backend.state != .running)
            if store.loading { ProgressView().controlSize(.small) }
        }.controlSize(.small)
    }
    private func row(_ event: DiagnosticOperationEvent) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(event.operation.label).fontWeight(.semibold)
                Text(event.stage.label).foregroundStyle(.secondary)
                Spacer()
                Text(event.result.label).foregroundStyle(event.result == .failed || event.result == .unknown ? Color.orange : Color.secondary)
            }.font(.callout)
            Text(event.code.label).font(.caption)
            Text("\(event.date.formatted(date: .abbreviated, time: .standard)) · \(event.durationMS) ms · 会话代次 \(event.generation == 0 ? "未分配" : String(event.generation)) · #\(event.sequence)")
                .font(.caption).foregroundStyle(.secondary)
            Text("关联 \(event.correlationID.prefix(12))")
                .font(.system(size: 10, design: .monospaced)).foregroundStyle(.secondary)
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 10))
    }
    private func refresh() {
        guard backend.state == .running else { feedback = "启动后端后可读取操作记录。"; return }
        cancel()
        feedback = nil
        request = Task { @MainActor in await store.refresh() }
    }
    private func cancel() { request?.cancel(); request = nil; store.invalidate() }
    private func export() {
        guard let snapshot = store.snapshot else { return }
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "Switch-safe-events.json"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            try snapshot.exportData().write(to: url, options: [.atomic])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            feedback = "安全操作记录已导出。"
        } catch { feedback = "导出未完成，请选择可写入的位置后重试。" }
    }
}
