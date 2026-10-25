import Combine
import Foundation

/// Owns module call-mode preparation and USB configuration backups.
///
/// Every asynchronous operation is tied to a lifecycle revision so replies
/// from a previous backend instance cannot overwrite the current UI state.
@MainActor
final class CallModeStore: ObservableObject {
    @Published var status: CallModeStatus?
    @Published var actionInFlight = false
    @Published var error: String?
    @Published var backups: [CallModeUSBBackupSummary] = []
    @Published var backupsLoading = false
    @Published var backupError: String?
    @Published var backupExportingID: String?
    @Published var backupImporting = false
    @Published var backupDeletingID: String?
    @Published var restoreInFlight = false

    var isCallActive: (() -> Bool)?
    var isAudioRunning: (() -> Bool)?
    var onStartAudio: (() -> Void)?
    var onToast: ((String, Bool, String?) -> Void)?

    private let backend: BackendProcess
    private var refreshInFlight = false
    private var observedRestoreAt: Date?
    private var lifecycleRevision: UInt64 = 0
    private var refreshRevision: UInt64 = 0
    private var backupsRevision: UInt64 = 0
    private var actionRevision: UInt64 = 0

    init(backend: BackendProcess) {
        self.backend = backend
    }

    var fileOperationInFlight: Bool {
        backupExportingID != nil || backupImporting || backupDeletingID != nil
    }

    func reset() {
        lifecycleRevision &+= 1
        refreshRevision &+= 1
        backupsRevision &+= 1
        actionRevision &+= 1
        refreshInFlight = false
        status = nil
        actionInFlight = false
        error = nil
        backups = []
        backupsLoading = false
        backupError = nil
        backupExportingID = nil
        backupImporting = false
        backupDeletingID = nil
        restoreInFlight = false
        observedRestoreAt = nil
    }

    func poll() {
        guard case .running = backend.state, !refreshInFlight else { return }
        refreshRevision &+= 1
        let requestRevision = refreshRevision
        let lifecycle = lifecycleRevision
        refreshInFlight = true
        Task {
            defer {
                if requestRevision == refreshRevision { refreshInFlight = false }
            }
            do {
                let previousBackupPath = status?.backupPath
                let response: CallModeStatus = try await APIClient(timeoutInterval: 12)
                    .get("api/call-mode/status")
                guard lifecycle == lifecycleRevision, requestRevision == refreshRevision else { return }
                status = response
                if response.isBusy || response.isReady { error = nil }
                if let backupPath = response.backupPath,
                   !backupPath.isEmpty,
                   backupPath != previousBackupPath {
                    loadBackups()
                }
                if let restore = response.lastRestore {
                    let isNewRestore = observedRestoreAt != restore.restoredAt
                    observedRestoreAt = restore.restoredAt
                    if isNewRestore && restoreInFlight {
                        restoreInFlight = false
                        loadBackups(force: true)
                        onToast?(
                            restore.changed
                                ? "模块原始 USB 配置已还原并完成重启验证。"
                                : "当前模块配置已经与所选备份一致。",
                            true,
                            "模块配置")
                    }
                }
                if response.isReady,
                   isCallActive?() == true,
                   isAudioRunning?() != true {
                    onStartAudio?()
                }
                if !response.isBusy {
                    actionInFlight = false
                    if response.state == "failed" { restoreInFlight = false }
                }
            } catch {
                guard lifecycle == lifecycleRevision, requestRevision == refreshRevision else { return }
                self.error = "读取通话模式失败：\(error.localizedDescription)"
            }
        }
    }

    func enable(confirmADBAuthorization: Bool) {
        performStatusAction(failurePrefix: "开启通话模式失败") {
            try await APIClient(timeoutInterval: 15).send(
                "api/call-mode/enable",
                body: CallModeEnableRequest(
                    confirm: true,
                    confirmADBAuthorization: confirmADBAuthorization))
        }
    }

    func downloadRuntime(source: String) {
        performStatusAction(failurePrefix: "下载运行时失败") {
            try await APIClient(timeoutInterval: 15).send(
                "api/call-mode/download",
                body: CallModeDownloadRequest(confirm: true, source: source))
        }
    }

    func retryPreparation() {
        performStatusAction(failurePrefix: "重新准备通话模式失败") {
            try await APIClient(timeoutInterval: 15).send("api/call-mode/retry")
        }
    }

    private func performStatusAction(
        failurePrefix: String,
        restore: Bool = false,
        operation: @escaping () async throws -> CallModeStatus
    ) {
        guard !actionInFlight else { return }
        let lifecycle = lifecycleRevision
        actionRevision &+= 1
        let requestRevision = actionRevision
        refreshRevision &+= 1
        refreshInFlight = false
        actionInFlight = true
        restoreInFlight = restore
        error = nil
        Task {
            do {
                let response = try await operation()
                guard lifecycle == lifecycleRevision, requestRevision == actionRevision else { return }
                status = response
                if !response.isBusy { actionInFlight = false }
            } catch {
                guard lifecycle == lifecycleRevision, requestRevision == actionRevision else { return }
                self.error = "\(failurePrefix)：\(error.localizedDescription)"
                if mutationFailureRequiresStatusReconciliation(error) {
                    // Ending the Swift waiter does not prove that the durable
                    // Rust operation failed or released its hardware owner.
                    // Keep both fences and reconcile /status; only a Rust
                    // non-busy/terminal projection may release them.
                    poll()
                } else {
                    actionInFlight = false
                    if restore { restoreInFlight = false }
                }
            }
        }
    }

    func loadBackups(force: Bool = false) {
        guard case .running = backend.state else { return }
        if backupsLoading && !force { return }
        backupsRevision &+= 1
        let requestRevision = backupsRevision
        let lifecycle = lifecycleRevision
        backupsLoading = true
        backupError = nil
        Task {
            defer {
                if lifecycle == lifecycleRevision, requestRevision == backupsRevision {
                    backupsLoading = false
                }
            }
            do {
                let response: CallModeUSBBackupListResponse = try await APIClient(timeoutInterval: 12)
                    .get("api/call-mode/backups")
                guard lifecycle == lifecycleRevision, requestRevision == backupsRevision else { return }
                backups = response.backups
            } catch {
                guard lifecycle == lifecycleRevision, requestRevision == backupsRevision else { return }
                backupError = "读取配置备份失败：\(error.localizedDescription)"
            }
        }
    }

    func exportBackup(_ backup: CallModeUSBBackupSummary, to destination: URL) {
        guard !fileOperationInFlight else { return }
        guard let encodedID = backup.id.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) else {
            backupError = "备份文件名无法安全编码。"
            return
        }
        let lifecycle = lifecycleRevision
        backupExportingID = backup.id
        backupError = nil
        Task {
            defer {
                if lifecycle == lifecycleRevision { backupExportingID = nil }
            }
            do {
                let data = try await APIClient(timeoutInterval: 20)
                    .getData("api/call-mode/backups/export?id=\(encodedID)")
                guard lifecycle == lifecycleRevision else { return }
                try data.write(to: destination, options: .atomic)
                onToast?("备份已保存到 \(destination.path)", true, "配置备份")
            } catch {
                guard lifecycle == lifecycleRevision else { return }
                backupError = "导出配置备份失败：\(error.localizedDescription)"
            }
        }
    }

    func importBackup(from source: URL) {
        guard !fileOperationInFlight else { return }
        let lifecycle = lifecycleRevision
        backupImporting = true
        backupError = nil
        Task {
            defer {
                if lifecycle == lifecycleRevision { backupImporting = false }
            }
            let accessGranted = source.startAccessingSecurityScopedResource()
            defer { if accessGranted { source.stopAccessingSecurityScopedResource() } }
            do {
                let values = try source.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                guard values.isRegularFile == true,
                      let fileSize = values.fileSize,
                      fileSize > 0,
                      fileSize <= 64 * 1_024 else {
                    guard lifecycle == lifecycleRevision else { return }
                    backupError = "导入配置备份失败：请选择不超过 64 KB 的 JSON 文件。"
                    return
                }
                let data = try Data(contentsOf: source, options: [.mappedIfSafe])
                let response: CallModeUSBBackupImportResponse = try await APIClient(timeoutInterval: 20)
                    .upload("api/call-mode/backups/import", data: data)
                guard lifecycle == lifecycleRevision else { return }
                loadBackups(force: true)
                onToast?(
                    "已导入 \(response.backup.savedAt.formatted(date: .numeric, time: .shortened)) 的模块配置备份。",
                    true,
                    "配置备份")
            } catch {
                guard lifecycle == lifecycleRevision else { return }
                backupError = "导入配置备份失败：\(error.localizedDescription)"
            }
        }
    }

    func deleteBackup(_ backup: CallModeUSBBackupSummary) {
        guard !fileOperationInFlight, !actionInFlight else { return }
        let lifecycle = lifecycleRevision
        backupDeletingID = backup.id
        backupError = nil
        Task {
            defer {
                if lifecycle == lifecycleRevision { backupDeletingID = nil }
            }
            do {
                let response: CallModeUSBBackupDeleteResponse = try await APIClient(timeoutInterval: 20).send(
                    "api/call-mode/backups/delete",
                    body: CallModeBackupDeleteRequest(confirm: true, backupID: backup.id))
                guard lifecycle == lifecycleRevision else { return }
                guard response.deleted else {
                    backupError = "删除配置备份失败：服务没有确认删除。"
                    return
                }
                backups.removeAll { $0.id == response.backupID }
                onToast?("配置备份已删除。", true, "配置备份")
            } catch {
                guard lifecycle == lifecycleRevision else { return }
                backupError = "删除配置备份失败：\(error.localizedDescription)"
            }
        }
    }

    func restoreBackup(_ backup: CallModeUSBBackupSummary) {
        performStatusAction(failurePrefix: "还原模块配置失败", restore: true) {
            try await APIClient(timeoutInterval: 15).send(
                "api/call-mode/restore",
                body: CallModeRestoreRequest(confirm: true, backupID: backup.id))
        }
    }
}
