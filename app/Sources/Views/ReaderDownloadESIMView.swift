import AppKit
import SwiftUI
import UniformTypeIdentifiers

private enum ActivationCodeMessageTone {
  case info
  case success
  case warning

  var systemImage: String {
    switch self {
    case .info: return "info.circle"
    case .success: return "checkmark.circle.fill"
    case .warning: return "exclamationmark.triangle.fill"
    }
  }

  var iconColor: Color {
    switch self {
    case .info: return .secondary
    case .success: return .green
    case .warning: return .orange
    }
  }

  var textColor: Color {
    switch self {
    case .info: return .secondary
    case .success, .warning: return .primary
    }
  }
}

/// 读卡器 eSIM 下载视图
struct ReaderDownloadESIMView: View {
  @ObservedObject var store: ReaderESIMStore

  var body: some View {
    DownloadESIMContent(
      overview: store.overview,
      operation: store.operation,
      banner: store.banner,
      isDownloading: store.action == .downloading,
      startDownload: { store.startDownload($0) },
      confirmDownload: { store.confirmDownload(operationID: $0, confirm: $1) })
  }
}

struct DownloadESIMContent: View {
  @Environment(\.dismiss) private var dismiss
  @Environment(\.workspaceViewport) private var viewport

  let overview: ESIMOverview?
  let operation: ESIMOperationSnapshot?
  let banner: ESIMBanner?
  let isDownloading: Bool
  let startDownload: (ESIMDownloadRequest) -> Void
  let confirmDownload: (String, Bool) -> Void
  var deviceLabel: String = "读卡器直写"
  var deviceName: String = "读卡器"

  @State private var activationCode = ""
  @State private var smdp = ""
  @State private var matchingID = ""
  @State private var confirmationCode = ""
  @State private var downloadIdentity = ""
  @State private var selectedAID = ""
  @State private var didStart = false
  @State private var operationIDBeforeStart: String?
  @State private var parseMessage: String?
  @State private var parseMessageTone: ActivationCodeMessageTone = .info
  @State private var showQRCodeImageImporter = false
  @State private var showQRCodeScanner = false
  @State private var isQRCodeDropTargeted = false
  @State private var isImportingQRCode = false
  @State private var suppressAutomaticParse = false
  @State private var feedbackContentHeight: CGFloat = 0

  private var downloadOperation: ESIMOperationSnapshot? {
    guard didStart,
      let op = operation,
      op.kind == "download_profile",
      op.id != operationIDBeforeStart
    else { return nil }
    return op
  }

  private var isSubmitting: Bool {
    isDownloading && downloadOperation == nil
  }

  private var isActive: Bool {
    isSubmitting || downloadOperation?.blocksNewMutation == true
  }

  private var targetGroups: [EUICCProfiles] {
    let groups = (overview?.profiles ?? []).filter { !($0.aidHex ?? "").isEmpty }
    guard overview?.chipInfo?.identity?.brand?.caseInsensitiveCompare("9eSIM") == .orderedSame else {
      return groups
    }
    // NekokoLPA2 tries the 9eSIM/eSTK-compatible SE1 AID before SE0.
    // Keep the explicit picker choice intact while making that order initial.
    return groups.sorted { lhs, rhs in
      func rank(_ aid: String?) -> Int {
        let value = aid?.uppercased() ?? ""
        if value.hasSuffix("31") { return 0 }
        if value.hasSuffix("30") { return 1 }
        return 2
      }
      return rank(lhs.aidHex) < rank(rhs.aidHex)
    }
  }

  var body: some View {
    VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
      WorkspaceHero(
        title: "下载 Profile",
        subtitle: "\(deviceLabel) · 写入期间请保持\(deviceName)连接",
        symbol: "arrow.down.to.line",
        eyebrow: "ESIM DOWNLOAD")

      ScrollView {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
          VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 10 : 14) {
            WorkspaceSectionHeader(title: "激活码", detail: "粘贴激活码，或从二维码识别连接信息")
            HStack {
              TextField("LPA:1$SM-DP+$Matching ID", text: $activationCode)
                .textFieldStyle(.workspace)
                .compatibleOnChange(of: activationCode) { value in
                  if !suppressAutomaticParse,
                    value.trimmingCharacters(in: .whitespacesAndNewlines)
                      .uppercased().hasPrefix("LPA:")
                  {
                    parseActivationCode(value, showErrors: false)
                  }
                }
              Button("解析") { parseActivationCode(showErrors: true) }
                .buttonStyle(.proletarius)
                .disabled(activationCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }

            HStack(spacing: 8) {
              Button(action: importFromPasteboard) {
                Label("从剪贴板", systemImage: "doc.on.clipboard")
              }
              Button {
                showQRCodeImageImporter = true
              } label: {
                Label("选择图片", systemImage: "photo")
              }
              Button {
                showQRCodeScanner = true
              } label: {
                Label("摄像头扫描", systemImage: "camera.viewfinder")
              }
              Spacer(minLength: 0)
            }
            .controlSize(.small)
            .buttonStyle(.proletarius)
            .disabled(isActive || isImportingQRCode)

            Label(
              isQRCodeDropTargeted ? "松开即可识别二维码" : "也可以把二维码图片或激活码拖到这里",
              systemImage: isQRCodeDropTargeted ? "arrow.down.circle.fill" : "square.and.arrow.down"
            )
            .font(.caption)
            .foregroundStyle(isQRCodeDropTargeted ? WorkspaceColors.accent : Color.secondary)

            if let parseMessage {
              HStack(alignment: .firstTextBaseline, spacing: 6) {
                if isImportingQRCode {
                  ProgressView()
                    .controlSize(.mini)
                } else {
                  Image(systemName: parseMessageTone.systemImage)
                    .foregroundStyle(parseMessageTone.iconColor)
                    .accessibilityHidden(true)
                }
                Text(parseMessage)
                  .foregroundStyle(parseMessageTone.textColor)
              }
              .font(.caption)
            }
          }
          .padding(viewport.isCompactHeight ? 14 : 18)
          .workspaceCard()
          .background(
            RoundedRectangle(cornerRadius: 20)
              .fill(WorkspaceColors.accent.opacity(isQRCodeDropTargeted ? 0.06 : 0))
          )
          .overlay(
            RoundedRectangle(cornerRadius: 20)
              .stroke(isQRCodeDropTargeted ? WorkspaceColors.accent : Color.clear, lineWidth: 2)
          )
          .onDrop(
            of: [UTType.image.identifier, UTType.fileURL.identifier, UTType.plainText.identifier],
            isTargeted: $isQRCodeDropTargeted,
            perform: handleQRCodeDrop)

          VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 10 : 14) {
            WorkspaceSectionHeader(title: "连接信息", detail: "解析后自动填写，也可以直接编辑下列参数")
            Grid(
              alignment: .leading, horizontalSpacing: 14,
              verticalSpacing: viewport.isCompactHeight ? 8 : 12
            ) {
              GridRow {
                Text("SM-DP+ 地址").foregroundStyle(.secondary)
                TextField("smdp.example.com", text: $smdp)
                  .textFieldStyle(.workspace)
              }
              GridRow {
                Text("Matching ID").foregroundStyle(.secondary)
                TextField("可选", text: $matchingID)
                  .textFieldStyle(.workspace)
              }
              GridRow {
                Text("确认码").foregroundStyle(.secondary)
                SecureField("可选", text: $confirmationCode)
                  .textFieldStyle(.workspace)
              }
              GridRow {
                Text("TAC / IMEI").foregroundStyle(.secondary)
                TextField("可选：8 位 TAC 或 15 位 IMEI", text: $downloadIdentity)
                  .textFieldStyle(.workspace)
              }
              if targetGroups.count > 1 {
                GridRow {
                  Text("目标 eUICC")
                  Picker("", selection: $selectedAID) {
                    ForEach(Array(targetGroups.enumerated()), id: \.offset) { index, group in
                      Text(targetTitle(group, index: index)).tag(group.aidHex ?? "")
                    }
                  }
                  .labelsHidden()
                }
              }
            }
            .font(.callout)
            Text("留空使用默认 8 位 TAC（35383741）。完整 IMEI 会包含在本次 eUICC AuthenticateServer 请求中，仅在服务商要求时填写。")
              .font(.caption)
              .foregroundStyle(.secondary)
          }
          .padding(viewport.isCompactHeight ? 14 : 18)
          .workspaceCard()

        }
        .padding(.bottom, 2)
      }

      downloadFeedback

      Divider().overlay(WorkspaceColors.outline)

      HStack(spacing: 12) {
        Button("取消") { dismiss() }
          .buttonStyle(.proletarius)
          .keyboardShortcut(.cancelAction)
          .disabled(isActive)
        Spacer()
        if let operation = downloadOperation, operation.awaitsDownloadConfirmation {
          Button("取消下载") { confirmDownload(operation.id, false) }
            .buttonStyle(.proletarius)
          Spacer()
          Button("确认并写入") { confirmDownload(operation.id, true) }
            .buttonStyle(.proletariusPrimary)
            .keyboardShortcut(.defaultAction)
        } else if let operation = downloadOperation, operation.isKnownTerminal {
          Button("完成") { dismiss() }
            .buttonStyle(.proletariusPrimary)
            .keyboardShortcut(.defaultAction)
        } else {
          Button {
            download()
          } label: {
            if isActive {
              ProgressView().controlSize(.small)
            } else {
              Text("下载并安装")
            }
          }
          .buttonStyle(.proletariusPrimary)
          .keyboardShortcut(.defaultAction)
          .disabled(isActive || smdp.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
      }
    }
    .padding(viewport.pageInset)
    .frame(
      width: min(620, max(480, viewport.width - 44)),
      height: min(660, max(420, viewport.height - 44))
    )
    .environment(\.workspaceAccent, WorkspaceColors.accent)
    .environment(\.workspaceVisualStyle, .nekoko)
    .proletariusPageBackground()
    .interactiveDismissDisabled(isActive)
    .fileImporter(
      isPresented: $showQRCodeImageImporter,
      allowedContentTypes: [.image],
      onCompletion: handleQRCodeFileSelection
    )
    .sheet(isPresented: $showQRCodeScanner) {
      ESIMQRCodeScannerSheet { code in
        applyImportedActivationCode(code, source: "摄像头")
      }
    }
    .onAppear {
      if selectedAID.isEmpty {
        selectedAID = targetGroups.first?.aidHex ?? ""
      }
    }
  }

  @ViewBuilder
  private var downloadFeedback: some View {
    if let operation = downloadOperation {
      boundedFeedback { operationStatus(operation) }
    } else if didStart, isSubmitting {
      boundedFeedback {
        HStack(spacing: 10) {
          ProgressView().controlSize(.small)
          Text("正在提交下载请求…")
            .font(.callout)
          Spacer(minLength: 0)
        }
        .padding(viewport.isCompactHeight ? 12 : 18)
      }
    } else if didStart, let banner {
      boundedFeedback {
        Label(banner.detail ?? banner.message, systemImage: "xmark.octagon.fill")
          .font(.callout)
          .foregroundStyle(.red)
          .fixedSize(horizontal: false, vertical: true)
          .padding(viewport.isCompactHeight ? 12 : 18)
      }
    }
  }

  private func boundedFeedback<Feedback: View>(@ViewBuilder content: () -> Feedback) -> some View {
    ScrollView {
      content()
        .frame(maxWidth: .infinity, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
        .background {
          GeometryReader { geometry in
            Color.clear.preference(key: DownloadFeedbackHeightKey.self, value: geometry.size.height)
          }
        }
    }
    .scrollIndicators(.visible)
    .frame(
      height: min(
        feedbackContentHeight > 0 ? feedbackContentHeight : 56,
        max(90, min(200, viewport.height * 0.30)))
    )
    .workspaceCard()
    .onPreferenceChange(DownloadFeedbackHeightKey.self) { height in
      guard height.isFinite, height > 0,
        abs(feedbackContentHeight - height) > 0.5
      else { return }
      feedbackContentHeight = height
    }
  }

  private func download() {
    operationIDBeforeStart = operation?.id
    didStart = true
    let request = ESIMDownloadRequest(
      smdp: smdp.trimmingCharacters(in: .whitespacesAndNewlines),
      matchingID: optionalTrimmed(matchingID),
      confirmationCode: optionalTrimmed(confirmationCode),
      aid: selectedAID.isEmpty ? nil : selectedAID,
      imei: optionalTrimmed(downloadIdentity))
    startDownload(request)
  }

  private func importFromPasteboard() {
    let pasteboard = NSPasteboard.general
    if let text = pasteboard.string(forType: .string),
      let code = ESIMQRCodeDecoder.activationCode(from: text)
    {
      applyImportedActivationCode(code, source: "剪贴板")
      return
    }

    if let data = pasteboard.data(forType: .png) ?? pasteboard.data(forType: .tiff) {
      recognizeQRCode(in: data, source: "剪贴板图片")
      return
    }

    if let image = NSImage(pasteboard: pasteboard), let data = image.tiffRepresentation {
      recognizeQRCode(in: data, source: "剪贴板图片")
      return
    }

    if let rawURL = pasteboard.string(forType: .fileURL), let url = URL(string: rawURL) {
      importQRCode(at: url, source: "剪贴板图片")
      return
    }

    setImportMessage("剪贴板中没有 eSIM 激活码或二维码图片", tone: .warning)
  }

  private func handleQRCodeFileSelection(_ result: Result<URL, any Error>) {
    switch result {
    case .success(let url):
      importQRCode(at: url, source: "所选图片")
    case .failure(let error):
      let nsError = error as NSError
      guard nsError.code != NSUserCancelledError else { return }
      setImportMessage("无法读取所选图片：\(error.localizedDescription)", tone: .warning)
    }
  }

  private func handleQRCodeDrop(_ providers: [NSItemProvider]) -> Bool {
    guard !isActive, !isImportingQRCode else { return false }

    if let provider = providers.first(where: {
      $0.hasItemConformingToTypeIdentifier(UTType.image.identifier)
    }) {
      provider.loadDataRepresentation(forTypeIdentifier: UTType.image.identifier) { data, error in
        DispatchQueue.main.async {
          if let data {
            recognizeQRCode(in: data, source: "拖入的图片")
          } else {
            setImportMessage(
              "无法读取拖入的图片：\(error?.localizedDescription ?? "未知错误")",
              tone: .warning)
          }
        }
      }
      return true
    }

    if let provider = providers.first(where: {
      $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier)
    }) {
      provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, error in
        let url: URL?
        if let item = item as? URL {
          url = item
        } else if let item = item as? NSURL {
          url = item as URL
        } else if let data = item as? Data {
          url = URL(dataRepresentation: data, relativeTo: nil)
        } else {
          url = nil
        }
        DispatchQueue.main.async {
          if let url {
            importQRCode(at: url, source: "拖入的图片")
          } else {
            setImportMessage(
              "无法读取拖入的文件：\(error?.localizedDescription ?? "文件格式不受支持")",
              tone: .warning)
          }
        }
      }
      return true
    }

    if let provider = providers.first(where: {
      $0.hasItemConformingToTypeIdentifier(UTType.plainText.identifier)
    }) {
      provider.loadItem(forTypeIdentifier: UTType.plainText.identifier, options: nil) {
        item, error in
        let text: String?
        if let item = item as? String {
          text = item
        } else if let item = item as? NSString {
          text = item as String
        } else if let data = item as? Data {
          text = String(data: data, encoding: .utf8)
        } else {
          text = nil
        }
        DispatchQueue.main.async {
          if let text, let code = ESIMQRCodeDecoder.activationCode(from: text) {
            applyImportedActivationCode(code, source: "拖入内容")
          } else {
            setImportMessage(
              error.map { "无法读取拖入内容：\($0.localizedDescription)" }
                ?? "拖入的内容不是 eSIM 激活码",
              tone: .warning)
          }
        }
      }
      return true
    }

    return false
  }

  private func importQRCode(at url: URL, source: String) {
    let hasSecurityScope = url.startAccessingSecurityScopedResource()
    defer {
      if hasSecurityScope {
        url.stopAccessingSecurityScopedResource()
      }
    }
    do {
      recognizeQRCode(in: try Data(contentsOf: url, options: .mappedIfSafe), source: source)
    } catch {
      setImportMessage("无法读取图片：\(error.localizedDescription)", tone: .warning)
    }
  }

  private func recognizeQRCode(in data: Data, source: String) {
    guard !isImportingQRCode else { return }
    isImportingQRCode = true
    setImportMessage("正在识别\(source)中的二维码…", tone: .info)

    Task {
      let result = await Task.detached(priority: .userInitiated) {
        () -> Result<String, ESIMQRCodeError> in
        do {
          return .success(try ESIMQRCodeDecoder.activationCode(in: data))
        } catch let error as ESIMQRCodeError {
          return .failure(error)
        } catch {
          return .failure(.recognitionFailed(error.localizedDescription))
        }
      }.value

      await MainActor.run {
        isImportingQRCode = false
        switch result {
        case .success(let code):
          applyImportedActivationCode(code, source: source)
        case .failure(let error):
          setImportMessage(error.localizedDescription, tone: .warning)
        }
      }
    }
  }

  private func applyImportedActivationCode(_ rawValue: String, source: String) {
    guard let code = ESIMQRCodeDecoder.activationCode(from: rawValue) else {
      setImportMessage("识别到的内容不是 eSIM 激活码", tone: .warning)
      return
    }
    suppressAutomaticParse = true
    activationCode = code
    parseActivationCode(code, showErrors: true, source: source)
    DispatchQueue.main.async {
      suppressAutomaticParse = false
    }
  }

  private func setImportMessage(_ message: String, tone: ActivationCodeMessageTone) {
    parseMessage = message
    parseMessageTone = tone
  }

  private func parseActivationCode(showErrors: Bool) {
    parseActivationCode(activationCode, showErrors: showErrors)
  }

  private func parseActivationCode(_ rawValue: String, showErrors: Bool, source: String? = nil) {
    let raw = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
    let parts = raw.split(separator: "$", omittingEmptySubsequences: false).map(String.init)
    guard parts.count >= 3, parts[0].uppercased() == "LPA:1" else {
      if showErrors {
        setImportMessage("激活码格式不正确，应以 LPA:1$ 开头", tone: .warning)
      }
      return
    }
    let parsedSMDP = parts[1].trimmingCharacters(in: .whitespacesAndNewlines)
    let parsedMatchingID = parts[2].trimmingCharacters(in: .whitespacesAndNewlines)
    guard !parsedSMDP.isEmpty, !parsedMatchingID.isEmpty else {
      if showErrors {
        setImportMessage("激活码缺少 SM-DP+ 地址或 Matching ID", tone: .warning)
      }
      return
    }
    smdp = parsedSMDP
    matchingID = parsedMatchingID
    let successMessage =
      source.map { "已从\($0)识别并解析激活码" }
      ?? "已解析 SM-DP+ 地址和 Matching ID"
    if parts.count > 3 {
      setImportMessage("\(successMessage)；扩展字段未自动用作确认码", tone: .warning)
    } else {
      setImportMessage(successMessage, tone: .success)
    }
  }

  @ViewBuilder
  private func operationStatus(_ operation: ESIMOperationSnapshot) -> some View {
    VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 8 : 12) {
      if operation.awaitsDownloadConfirmation {
        Label("Profile 下载预览", systemImage: "doc.zipper")
          .font(.callout.weight(.semibold))
          .foregroundStyle(WorkspaceColors.accent)
        if let size = operation.estimatedProfileSizeBytes, size > 0 {
          Text("预计配置文件大小：\(ByteCountFormatter.string(fromByteCount: size, countStyle: .file))")
            .font(.callout)
        } else {
          Text("暂无可靠的大小估算；确认后才会开始获取配置文件。")
            .font(.callout)
            .foregroundStyle(.secondary)
        }
        Text("确认后才会继续获取配置包并写入 eUICC。")
          .font(.caption)
          .foregroundStyle(.secondary)
      } else if operation.isActive {
        HStack {
          Text(operation.message ?? "正在下载 Profile…")
            .font(.callout.weight(.medium))
          Spacer()
          Text("\(operation.progress)%")
            .font(.callout.monospacedDigit().weight(.semibold))
            .foregroundStyle(WorkspaceColors.accent)
        }
        ProgressView(value: Double(operation.progress), total: 100)
          .tint(WorkspaceColors.accent)
        if let step = operation.step, !step.isEmpty {
          Text(downloadStageTitle(step))
            .font(.caption)
            .foregroundStyle(.secondary)
        }
      } else if operation.isFailed {
        VStack(alignment: .leading, spacing: 5) {
          Label(operation.error ?? "Profile 下载失败", systemImage: "xmark.octagon.fill")
            .foregroundStyle(.red)
          if let step = operation.step, !step.isEmpty, step != "failed" {
            Text("失败阶段：\(downloadStageTitle(step))")
              .foregroundStyle(.secondary)
          }
          if let code = operation.errorCode, !code.isEmpty {
            Text("错误码：\(code)")
              .font(.caption.monospaced())
              .foregroundStyle(.secondary)
          }
          if let diagnostic = operation.diagnostic?.trimmingCharacters(in: .whitespacesAndNewlines),
            !diagnostic.isEmpty
          {
            Text("诊断信息：\(diagnostic)")
              .textSelection(.enabled)
              .foregroundStyle(.secondary)
              .fixedSize(horizontal: false, vertical: true)
          }
        }
      } else if operation.hasWarning {
        Label(
          operation.result?.warning ?? operation.message ?? "下载完成，但需要检查卡片状态",
          systemImage: "exclamationmark.triangle.fill"
        )
        .foregroundStyle(.orange)
      } else if operation.isCancelled {
        Label(operation.message ?? "已取消下载", systemImage: "xmark.circle")
          .foregroundStyle(.secondary)
      } else if operation.isSuccessful {
        Label(operation.message ?? "Profile 下载完成", systemImage: "checkmark.circle.fill")
          .foregroundStyle(.green)
      } else {
        Label("操作结果待确认（\(operation.state)）", systemImage: "exclamationmark.triangle.fill")
          .foregroundStyle(.orange)
      }
    }
    .font(.callout)
    .padding(viewport.isCompactHeight ? 12 : 18)
    .frame(maxWidth: .infinity, alignment: .leading)
  }

  private func downloadStageTitle(_ step: String) -> String {
    switch step {
    case "preflight": return "设备预检"
    case "init_auth": return "向 SM-DP+ 发起认证"
    case "auth_euicc": return "eUICC 验证 SM-DP+ 签名"
    case "auth_client": return "SM-DP+ 客户端认证"
    case "auth_server": return "SM-DP+ Profile 获取"
    case "confirm_download": return "等待确认下载"
    case "install": return "写入 eUICC"
    case "notify": return "运营商安装通知"
    default: return step
    }
  }

  private func targetTitle(_ group: EUICCProfiles, index: Int) -> String {
    guard let eid = group.eid, !eid.isEmpty else { return "eUICC \(index + 1)" }
    return "eUICC \(index + 1) · …\(eid.suffix(8))"
  }

  private func optionalTrimmed(_ value: String) -> String? {
    let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? nil : trimmed
  }
}

private struct DownloadFeedbackHeightKey: PreferenceKey {
  static let defaultValue: CGFloat = 0

  static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
    value = max(value, nextValue())
  }
}
