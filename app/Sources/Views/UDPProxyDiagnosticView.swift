import SwiftUI

/// Runs only on an explicit click. It never scans readers, starts IMS, sends
/// carrier traffic, persists the destination, or retries with a direct socket.
struct UDPProxyDiagnosticView: View {
    @EnvironmentObject private var backend: BackendProcess
    @Environment(\.workspaceViewport) private var viewport
    @State private var proxy = ""
    @State private var dnsServer = ""
    @State private var dnsName = ""
    @State private var completedProbe: UDPProxyDiagnosticResultSnapshot?
    @State private var error: String?
    @State private var task: Task<Void, Never>?
    @State private var revision: UInt64 = 0
    @State private var busy = false

    private var canProbe: Bool {
        backend.state == .running && !proxy.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var inputs: UDPProxyDiagnosticInputs {
        .init(proxy: proxy, dnsServer: dnsServer, dnsName: dnsName)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
                HStack(spacing: 12) {
                    WorkspaceSymbol(systemName: "arrow.left.arrow.right", tint: WorkspaceColors.accent, size: 40)
                    WorkspaceSectionHeader(title: "代理 UDP 验证", detail: "分别验证握手、Associate 与实际数据往返。")
                    Spacer()
                    if busy {
                        Button("停止") { cancel() }.controlSize(.small)
                        ProgressView().controlSize(.small)
                    }
                }
                Text("输入 SOCKS5 代理；只有点击“验证 UDP 往返”才会向指定 DNS 服务器发送一次 A 查询。未内置公共目的，域名通过代理传递；不直连降级。")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                VStack(alignment: .leading, spacing: 10) {
                    input("SOCKS5 代理", placeholder: "主机:端口", text: $proxy)
                    input("DNS 探测目的", placeholder: "IPv4:端口 或 [IPv6]:端口", text: $dnsServer)
                    input("DNS 查询域名", placeholder: "填写要查询的域名", text: $dnsName)
                    HStack(spacing: 10) {
                        Button { probe(roundtrip: false) } label: {
                            Label("只检查握手", systemImage: "network")
                        }.disabled(busy || !canProbe)
                        Button { probe(roundtrip: true) } label: {
                            Label("验证 UDP 往返", systemImage: "arrow.left.arrow.right")
                        }.disabled(busy || !canProbe || dnsServer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || dnsName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }.controlSize(.small)
                }
                if let result = completedProbe?.result(for: inputs) {
                    Divider()
                    LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 10), count: viewport.columnCount(minimumWidth: 170, maximum: 4)), spacing: 10) {
                        evidence("TCP 连接", result.tcpConnected)
                        evidence("SOCKS5 握手", result.authenticated)
                        evidence("UDP Associate", result.udpAssociate)
                        evidence("实际 UDP 往返", result.roundtripEvidence)
                    }
                    Text(result.summary)
                        .font(.callout).foregroundStyle(result.confirmedRoundtrip ? Color.primary : Color.orange)
                        .fixedSize(horizontal: false, vertical: true)
                    if let duration = result.latencyMS {
                        Text("本次检测 \(duration) ms" + (result.roundtripLatencyMS.map { " · UDP 往返 \($0) ms" } ?? ""))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                if let error {
                    Text(error).font(.caption).foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text("结果仅适用于本次代理与 DNS 目的。DNS 往返不代表 ePDG、IKE、ESP 或 IMS/SMS 已就绪；运营商状态请查看读卡器与 N待。")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(viewport.cardPadding)
            .workspaceCard()
        }
        .onDisappear { cancel() }
        .compatibleOnChange(of: backend.state) { state in
            if state != .running { cancel(); error = "后端未运行，保留上次完成的检测结果。" }
        }
    }

    private func input(_ title: String, placeholder: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            TextField(placeholder, text: text).textFieldStyle(.roundedBorder).disabled(busy)
        }
    }

    private func evidence(_ title: String, _ value: Bool?) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Label(value.map { $0 ? "已确认" : "未确认" } ?? "未检测", systemImage: value.map { $0 ? "checkmark.circle" : "exclamationmark.circle" } ?? "minus.circle")
                .font(.callout).foregroundStyle(value == true ? Color.green : Color.secondary)
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 10))
    }

    private func probe(roundtrip: Bool) {
        guard !busy, canProbe else { return }
        revision &+= 1
        let requestRevision = revision
        let testedInputs = inputs
        let request = UDPProxyProbeRequest(proxy: proxy.trimmingCharacters(in: .whitespacesAndNewlines),
            udpDNS: roundtrip ? .init(server: dnsServer.trimmingCharacters(in: .whitespacesAndNewlines), name: dnsName.trimmingCharacters(in: .whitespacesAndNewlines)) : nil)
        busy = true
        error = nil
        task = Task { @MainActor in
            do {
                let response: UDPProxyProbeResult = try await APIClient(timeoutInterval: 10).send("api/v2/vowifi/proxy-probe", body: request)
                guard !Task.isCancelled, requestRevision == revision else { return }
                completedProbe = .init(inputs: testedInputs, value: response)
                busy = false
                task = nil
            } catch {
                guard !Task.isCancelled, requestRevision == revision else { return }
                // Keep raw addresses and nested transport errors out of the UI.
                self.error = "检测未完成：请检查代理地址、DNS 的 IP:端口与域名后重试。"
                busy = false
                task = nil
            }
        }
    }

    private func cancel() {
        revision &+= 1
        task?.cancel()
        task = nil
        if busy { error = "检测已停止，保留上次完成的结果。" }
        busy = false
    }
}
