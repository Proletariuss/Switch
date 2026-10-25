import Combine
import Foundation

/// 读卡器 eSIM Profile 管理 Store
///
/// - 所有 API 调用走 `/api/reader/esim/*`（PC/SC 读卡器通道）
/// - Profile 切换后不重启模块、不监控模块 ICCID 变化
/// - 复用 ESIMOverview/ProfileItem/ESIMOperationSnapshot 等模型（后端响应格式相同）
@MainActor
final class ReaderESIMStore: ObservableObject {
    struct LegacyRecoveryCandidate: Identifiable {
        let snapshot: ESIMOperationSnapshot
        let rawUpdatedAt: String
        var id: String { snapshot.id }
    }

    struct ReaderContext: Equatable {
        let name: String?
        let connected: Bool
        let hasESIM: Bool
    }

    enum LoadPhase: Equatable {
        case idle
        case loading
        case loaded
        case failed(String)
    }

    enum Action: Equatable {
        case switching(String)
        case deleting(String)
        case renaming(String)
        case downloading
    }

    @Published private(set) var overview: ESIMOverview?
    @Published private(set) var loadPhase: LoadPhase = .idle
    @Published private(set) var isRefreshing = false
    @Published private(set) var refreshOutcome: ESIMRefreshOutcome?
    @Published private(set) var action: Action?
    @Published private(set) var operation: ESIMOperationSnapshot?
    /// Global operation journal state is independent of the selected reader.
    /// Legacy journals have no provable card owner, so the card-scoped overview
    /// intentionally does not contain this interrupted operation.
    @Published private(set) var legacyRecovery: LegacyRecoveryCandidate?
    @Published private(set) var legacyRecoveryStatusFresh = false
    @Published private(set) var isAcknowledgingLegacyRecovery = false
    @Published private(set) var legacyRecoveryError: String?
    @Published private(set) var legacyRecoveryNotice: String?
    @Published private(set) var profileNotes: [String: ProfileNote] = [:]
    @Published private(set) var notifications: [ESIMNotificationItem] = []
    @Published private(set) var isLoadingNotifications = false
    @Published private(set) var retryingNotificationID: String?
    @Published private(set) var notificationError: String?
    @Published var banner: ESIMBanner?

    private var loadTask: Task<Void, Never>?
    private var notesTask: Task<Void, Never>?
    private var actionTask: Task<Void, Never>?
    private var notificationTask: Task<Void, Never>?
    private var retryNotificationTask: Task<Void, Never>?
    private var operationPollTask: Task<Void, Never>?
    private var terminalRefreshTask: Task<Void, Never>?
    private var loadGeneration = 0
    private var legacyRecoveryReadSequence = 0
    private var sessionGeneration: UInt64 = 0
    private var lastHandledOperationID: String?
    private var synchronizedSwitchOperationID: String?
    private var unconfirmedSwitchICCID: String?
    private var readerContext: ReaderContext?
    private var loadErrorBannerID: UUID?
    private let mutationCheckpoints: ESIMMutationCheckpointStore
    private let notificationMutationCheckpoints: ESIMNotificationMutationCheckpointStore
    private var pendingMutation: ESIMMutationCheckpoint?
    private var pendingDownloadOperationID: String?
    private var corruptMutationCheckpoint: String?

    init(
        mutationCheckpoints: ESIMMutationCheckpointStore = ESIMMutationCheckpointStore(),
        notificationMutationCheckpoints: ESIMNotificationMutationCheckpointStore = ESIMNotificationMutationCheckpointStore()
    ) {
        self.mutationCheckpoints = mutationCheckpoints
        self.notificationMutationCheckpoints = notificationMutationCheckpoints
        switch mutationCheckpoints.load(scope: "reader") {
        case .missing: break
        case .valid(let checkpoint): pendingMutation = checkpoint
        case .corrupt(let diagnostic): corruptMutationCheckpoint = diagnostic
        }
        if corruptMutationCheckpoint != nil { showCorruptMutationCheckpoint() }
        if pendingMutation != nil {
            action = .downloading
            banner = ESIMBanner(
                tone: .warning,
                message: "eSIM 操作结果待确认",
                detail: "正在按原请求编号恢复 Rust 回执；写卡操作保持锁定")
        }
        if case .corrupt(let diagnostic) = notificationMutationCheckpoints.load() {
            notificationError = "运营商通知请求记录损坏：\(diagnostic)"
        }
    }

    deinit {
        loadTask?.cancel()
        notesTask?.cancel()
        actionTask?.cancel()
        notificationTask?.cancel()
        retryNotificationTask?.cancel()
        operationPollTask?.cancel()
        terminalRefreshTask?.cancel()
    }

    var capabilities: ESIMCapabilities? { overview?.capabilities }
    var isPhysicalSIM: Bool { overview?.cardType == "physical_sim" }
    var isInitialLoading: Bool { loadPhase == .loading && overview == nil }
    var isBusy: Bool {
        isRefreshing || action != nil || pendingMutation != nil || corruptMutationCheckpoint != nil || operation?.blocksNewMutation == true ||
            isLoadingNotifications || retryingNotificationID != nil
    }

    var profileGroups: [(group: EUICCProfiles, profiles: [ESIMProfileContext])] {
        (overview?.profiles ?? []).map { group in
            let eid = group.eid ?? ""
            let aid = group.aidHex ?? ""
            return (group, (group.profiles ?? []).map {
                ESIMProfileContext(eid: eid, aidHex: aid, profile: $0)
            })
        }
    }

    var profileCount: Int {
        profileGroups.reduce(0) { $0 + $1.profiles.count }
    }

    func savedPhoneNumber(for iccid: String) -> String? {
        let target = normalizedICCID(iccid)
        guard !target.isEmpty else { return nil }
        return usableSIMPhoneNumber(profileNotes.first(where: { normalizedICCID($0.key) == target })?.value.phone)
    }

    private func normalizedICCID(_ value: String) -> String {
        value.filter(\.isNumber)
    }

    // MARK: - API 路径前缀（读卡器 eSIM 专用）

    private let apiPrefix = "api/reader/esim"

    // MARK: - 无卡时仍可查看的旧版操作日志人工核对

    /// Read-only polling is scoped to the visible reader page. It never
    /// acknowledges a journal or starts a card operation.
    func observeLegacyRecovery() async {
        while !Task.isCancelled {
            await refreshLegacyRecovery()
            try? await Task.sleep(nanoseconds: 10_000_000_000)
        }
    }

    func refreshLegacyRecovery() async {
        guard !isAcknowledgingLegacyRecovery else { return }
        legacyRecoveryReadSequence &+= 1
        let sequence = legacyRecoveryReadSequence
        do {
            let response: ESIMOperationResponse = try await APIClient(timeoutInterval: 10)
                .get("api/esim/operation")
            guard !Task.isCancelled, sequence == legacyRecoveryReadSequence else { return }
            legacyRecoveryStatusFresh = true
            if let snapshot = response.operation,
               response.manualAckAvailable == true,
               snapshot.state == "failed",
               snapshot.errorCode == "esim_operation_interrupted",
               snapshot.recoverable != true,
               let rawUpdatedAt = response.rawUpdatedAt, !rawUpdatedAt.isEmpty {
                legacyRecovery = LegacyRecoveryCandidate(snapshot: snapshot, rawUpdatedAt: rawUpdatedAt)
            } else {
                legacyRecovery = nil
            }
            legacyRecoveryError = nil
        } catch {
            guard !Task.isCancelled, sequence == legacyRecoveryReadSequence else { return }
            legacyRecoveryStatusFresh = false
            if legacyRecovery != nil {
                legacyRecoveryError = "暂时无法读取操作状态：\(error.localizedDescription)"
            }
        }
    }

    /// Called only from the second, explicit confirmation action in the UI.
    /// A stale operation or timestamp requires the user to review again.
    func acknowledgeLegacyRecovery(
        _ reviewed: LegacyRecoveryCandidate,
        reviewedByUser: Bool,
        enteredIDSuffix: String
    ) async -> Bool {
        guard !hasCorruptMutationCheckpoint() else { return false }
        guard !isAcknowledgingLegacyRecovery else { return false }
        guard ESIMLegacyRecoveryConfirmation.canSubmit(
            reviewed: reviewedByUser,
            enteredIDSuffix: enteredIDSuffix,
            operationID: reviewed.id) else {
            legacyRecoveryError = "请先核对卡片状态，并输入操作编号末尾字符。"
            return false
        }
        guard legacyRecoveryStatusFresh,
              let current = legacyRecovery,
              current.id == reviewed.id,
              current.rawUpdatedAt == reviewed.rawUpdatedAt else {
            legacyRecoveryError = "操作状态已变化，请重新打开核对窗口。"
            return false
        }
        legacyRecoveryReadSequence &+= 1 // Discard any read started before the write.
        isAcknowledgingLegacyRecovery = true
        legacyRecoveryError = nil
        let request = ESIMLegacyRecoveryAcknowledgement(
            operationID: reviewed.id,
            expectedUpdatedAt: reviewed.rawUpdatedAt)
        do {
            let confirmed: ESIMOperationSnapshot = try await APIClient(timeoutInterval: 15)
                .send("api/esim/operation/acknowledge", body: request)
            isAcknowledgingLegacyRecovery = false
            guard confirmed.id == reviewed.id,
                  confirmed.state == "failed",
                  confirmed.errorCode == "esim_operation_interrupted",
                  confirmed.recoverable == true else {
                await refreshLegacyRecovery()
                legacyRecoveryError = "服务响应未能确认人工核对结果，请查看最新状态，切勿直接重试写卡。"
                return false
            }
            legacyRecovery = nil
            legacyRecoveryNotice = "已记录人工核对；上次 eSIM 操作结果仍未知。重新写卡前请检查 Profile 列表。"
            await refreshLegacyRecovery()
            return true
        } catch {
            isAcknowledgingLegacyRecovery = false
            await refreshLegacyRecovery()
            legacyRecoveryError = "人工核对确认未得到明确结果：\(error.localizedDescription)。请查看最新状态，切勿直接重试写卡。"
            return false
        }
    }

    // MARK: - 加载概览

    /// A USB/BLE selection can change without either readiness flag turning
    /// false. Invalidate the old reader before starting any new overview work.
    /// Repeated status polls with the same context do not re-read the eUICC.
    @discardableResult
    func synchronizeReader(_ context: ReaderContext) -> Bool {
        let previous = readerContext
        if previous == context {
            let hasSelectedReader = !(context.name?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
            if context.connected && context.hasESIM && hasSelectedReader,
               let pendingMutation, actionTask == nil, operation == nil {
                recoverPendingMutationOnLoad(pendingMutation)
            }
            return false
        }
        let invalidated = previous.map {
            $0.name != context.name || ($0.connected && !context.connected)
        } ?? false
        if invalidated { clearForReaderDisconnect() }
        readerContext = context
        let hasSelectedReader = !(context.name?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
        let shouldRecoverCheckpoint = pendingMutation != nil && !invalidated && actionTask == nil
        if context.connected && context.hasESIM && hasSelectedReader &&
            overview == nil && loadTask == nil &&
            (shouldRecoverCheckpoint || (action == nil && operation?.blocksNewMutation != true)) {
            load(recoverPendingMutation: !invalidated)
        }
        return invalidated
    }

    func load(forceRefresh: Bool = false, recoverPendingMutation: Bool = true) {
        guard !hasCorruptMutationCheckpoint() else { return }
        if recoverPendingMutation, let pendingMutation {
            recoverPendingMutationOnLoad(pendingMutation)
            return
        }
        loadTask?.cancel()
        loadGeneration &+= 1
        let generation = loadGeneration
        if forceRefresh {
            isRefreshing = true
            refreshOutcome = nil
        } else if overview == nil {
            loadPhase = .loading
        }

        loadTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.loadGeneration {
                    self.isRefreshing = false
                    self.loadTask = nil
                }
            }
            do {
                let path = forceRefresh ? "\(apiPrefix)?refresh=true" : apiPrefix
                let fetched: ESIMOverview = try await APIClient(timeoutInterval: forceRefresh ? 145 : 45).get(path)
                guard !Task.isCancelled, generation == loadGeneration else { return }
                refreshOutcome = fetched.refresh
                overview = fetched
                publishActiveProfileOperator(from: fetched)
                loadProfileNotes(generation: generation)
                loadPhase = .loaded
                if let loadErrorBannerID, banner?.id == loadErrorBannerID {
                    banner = nil
                }
                loadErrorBannerID = nil
                if let refresh = fetched.refresh, !refresh.completed {
                    banner = ESIMBanner(tone: .warning, message: refresh.message, detail: nil)
                }
                let retainedID = pendingDownloadOperationID ??
                    (operation?.blocksNewMutation == true ? operation?.id : nil)
                if let retainedID, fetched.operation?.id != retainedID {
                    banner = ESIMBanner(
                        tone: .warning,
                        message: "eSIM 操作结果待确认",
                        detail: "概览未返回原操作编号；写卡操作保持锁定")
                } else if pendingMutation?.action == "download", retainedID == nil {
                    banner = ESIMBanner(
                        tone: .warning,
                        message: "eSIM 操作结果待确认",
                        detail: "须按原请求编号恢复下载回执；写卡操作保持锁定")
                } else if let observedOperation = fetched.operation {
                    operation = observedOperation
                } else if operation?.blocksNewMutation == true {
                    banner = ESIMBanner(
                        tone: .warning,
                        message: "eSIM 操作结果待确认",
                        detail: "服务暂未返回原操作状态；写卡操作仍保持锁定")
                } else {
                    operation = nil
                }
                if let observedOperation = fetched.operation,
                   (retainedID == nil || observedOperation.id == retainedID),
                   pendingMutation?.action != "download" || retainedID != nil {
                    if observedOperation.blocksNewMutation {
                        trackOperation(observedOperation)
                    } else if observedOperation.isKnownTerminal,
                              pendingMutation?.action == "download" {
                        handleTerminalOperation(observedOperation)
                    } else if observedOperation.errorCode == "esim_operation_interrupted" {
                        if observedOperation.recoverable == true {
                            banner = ESIMBanner(
                                tone: .warning,
                                message: "Profile 列表已刷新，请核对当前卡片状态",
                                detail: observedOperation.error)
                        } else {
                            handleTerminalOperation(observedOperation)
                        }
                    }
                }
            } catch {
                guard !Task.isCancelled, generation == loadGeneration else { return }
                refreshOutcome = (error as? any ESIMRefreshFailure)?.refreshOutcome
                if overview == nil {
                    loadPhase = .failed(error.localizedDescription)
                } else {
                    let loadError = ESIMBanner(
                        tone: .error,
                        message: forceRefresh ? "刷新读卡器卡片信息失败" : "更新读卡器卡片信息失败",
                        detail: error.localizedDescription)
                    banner = loadError
                    loadErrorBannerID = loadError.id
                }
            }
        }
    }

    private func loadProfileNotes(generation: Int) {
        notesTask?.cancel()
        notesTask = Task { [weak self] in
            guard let self else { return }
            let result: NotesResponse? = try? await APIClient(timeoutInterval: 8).get("api/esim/notes")
            guard !Task.isCancelled, generation == loadGeneration else { return }
            profileNotes = result?.notes ?? [:]
            notesTask = nil
        }
    }

    func refresh() {
        guard capabilities?.canRefresh == true, !isBusy else { return }
        banner = nil
        load(forceRefresh: true)
    }

    /// Invalidates every reader-backed eSIM snapshot after removal or a
    /// USB/BLE reader selection change. User-entered settings live elsewhere
    /// and are intentionally not touched.
    func clearForReaderDisconnect() {
        loadTask?.cancel()
        notesTask?.cancel()
        actionTask?.cancel()
        notificationTask?.cancel()
        retryNotificationTask?.cancel()
        operationPollTask?.cancel()
        terminalRefreshTask?.cancel()
        loadTask = nil
        notesTask = nil
        actionTask = nil
        notificationTask = nil
        retryNotificationTask = nil
        operationPollTask = nil
        terminalRefreshTask = nil
        loadGeneration &+= 1
        sessionGeneration &+= 1
        overview = nil
        refreshOutcome = nil
        profileNotes = [:]
        loadPhase = .idle
        isRefreshing = false
        action = nil
        operation = nil
        pendingDownloadOperationID = nil
        banner = nil
        loadErrorBannerID = nil
        lastHandledOperationID = nil
        synchronizedSwitchOperationID = nil
        unconfirmedSwitchICCID = nil
        notifications = []
        isLoadingNotifications = false
        retryingNotificationID = nil
        notificationError = nil
        if corruptMutationCheckpoint != nil { showCorruptMutationCheckpoint() }
    }

    // MARK: - 运营商 Profile 通知

    func loadNotifications() {
        guard !isLoadingNotifications, retryingNotificationID == nil, action == nil else { return }
        let generation = sessionGeneration
        isLoadingNotifications = true
        notificationError = nil
        notificationTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.sessionGeneration {
                    self.isLoadingNotifications = false
                    self.notificationTask = nil
                }
            }
            do {
                let response: ESIMNotificationListResponse = try await APIClient(timeoutInterval: 45).get(
                    "\(apiPrefix)/notifications")
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                notifications = response.notifications
            } catch {
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                notificationError = error.localizedDescription
            }
        }
    }

    func retryNotification(_ item: ESIMNotificationItem) {
        guard !hasCorruptMutationCheckpoint() else { return }
        guard !isLoadingNotifications, retryingNotificationID == nil, action == nil, item.canRetry else { return }
        guard let checkpoint = notificationMutationCheckpoints.begin(item) else {
            notificationError = "运营商通知请求记录损坏，或另一条通知结果仍待确认；未发送新请求"
            return
        }
        let generation = sessionGeneration
        retryingNotificationID = item.id
        notificationError = nil
        retryNotificationTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.sessionGeneration {
                    self.retryingNotificationID = nil
                    self.retryNotificationTask = nil
                }
            }
            do {
                let _: MessageResponse = try await APIClient(timeoutInterval: 45).send(
                    "\(apiPrefix)/notifications/retry",
                    body: ESIMNotificationRetryRequest(
                        clientRequestID: checkpoint.clientRequestID,
                        sequenceNumber: checkpoint.sequenceNumber,
                        aid: checkpoint.aid))
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                guard notificationMutationCheckpoints.clear(checkpoint) else {
                    notificationError = "运营商通知已响应，但本机请求记录清除未确认；已保留操作围栏"
                    return
                }
                notifications.removeAll { $0.id == item.id }
                banner = ESIMBanner(
                    tone: .success,
                    message: "运营商通知已确认",
                    detail: nil)
            } catch {
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                notificationError = error.localizedDescription
            }
        }
    }

    // MARK: - Profile 切换

    /// A completed operation may still own a delayed overview refresh. A new
    /// user mutation supersedes that read (and any older metadata load), not
    /// the cached list itself. Cancelled replies remain fenced by loadGeneration.
    private func beginProfileAction(_ next: Action) {
        terminalRefreshTask?.cancel()
        terminalRefreshTask = nil
        operationPollTask?.cancel()
        operationPollTask = nil
        loadTask?.cancel()
        loadTask = nil
        notesTask?.cancel()
        notesTask = nil
        loadGeneration &+= 1
        isRefreshing = false
        operation = nil
        action = next
        banner = nil
    }

    func switchProfile(_ context: ESIMProfileContext) {
        guard capabilities?.canSwitch == true, !isBusy, !context.profile.isEnabled else { return }
        guard let checkpoint = beginMutation("switch") else { return }
        let generation = sessionGeneration
        beginProfileAction(.switching(context.id))
        actionTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.sessionGeneration { self.actionTask = nil }
            }
            do {
                let result: ESIMSwitchResult = try await APIClient(timeoutInterval: 130).send(
                    "\(apiPrefix)/switch",
                    body: ESIMSwitchRequest(
                        iccid: context.profile.iccid,
                        aid: context.aidHex,
                        clientRequestID: checkpoint.clientRequestID))
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                action = nil
                guard finishMutation(checkpoint) else { return }
                // An accepted switch may still report the old USIM identity.
                // Profile enabled-state alone does not prove activation on the
                // reader; only publish the requested operator after that read.
                let target = normalizedICCID(context.profile.iccid)
                let actual = normalizedICCID(result.readerInfo?.usimInfo?.iccid ?? "")
                let identityConfirmed = !target.isEmpty && actual == target
                unconfirmedSwitchICCID = identityConfirmed ? nil : target
                if identityConfirmed {
                    let carrierName = context.profile.serviceProviderName ?? context.profile.name
                    DashboardStore.shared?.updateReaderProfileOperatorName(carrierName)
                }
                DashboardStore.shared?.synchronizeReaderAfterProfileSwitch(result.readerInfo)
                if let op = result.operation {
                    synchronizedSwitchOperationID = op.id
                    trackOperation(op)
                } else {
                    banner = ESIMBanner(
                        tone: identityConfirmed ? .success : .warning,
                        message: identityConfirmed ? "Profile 切换完成" : "Profile 切换已受理，激活结果待确认",
                        detail: nil)
                    load(forceRefresh: true)
                }
            } catch {
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                if operationOutcomeIsUnknown(error) {
                    // A lost write response cannot prove either cancellation
                    // or USIM activation. Retain the same identity gate used
                    // for an explicitly accepted, unconfirmed switch.
                    unconfirmedSwitchICCID = normalizedICCID(context.profile.iccid)
                    await recoverMutation(checkpoint, title: "切换 Profile", generation: generation)
                    return
                }
                action = nil
                guard finishMutation(checkpoint) else { return }
                banner = ESIMBanner(tone: .error,
                                    message: "切换 Profile 失败",
                                    detail: error.localizedDescription)
            }
        }
    }

    // MARK: - Profile 删除

    func deleteProfile(_ context: ESIMProfileContext) {
        guard capabilities?.canDelete == true, !isBusy, !context.profile.isEnabled else { return }
        guard let checkpoint = beginMutation("delete") else { return }
        let generation = sessionGeneration
        beginProfileAction(.deleting(context.id))
        actionTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.sessionGeneration {
                    self.actionTask = nil
                }
            }
            do {
                let result: ESIMProfileResult = try await APIClient(timeoutInterval: 90).send(
                    "\(apiPrefix)/profile", method: "DELETE",
                    body: ESIMDeleteRequest(
                        iccid: context.profile.iccid,
                        aid: context.aidHex,
                        clientRequestID: checkpoint.clientRequestID))
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                action = nil
                guard finishMutation(checkpoint) else { return }
                if let warning = result.warning, !warning.isEmpty {
                    banner = ESIMBanner(tone: .warning, message: "Profile 已删除", detail: warning)
                } else {
                    banner = ESIMBanner(tone: .success, message: "Profile 已删除", detail: nil)
                }
                load(forceRefresh: true)
            } catch {
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                if operationOutcomeIsUnknown(error) {
                    await recoverMutation(checkpoint, title: "删除 Profile", generation: generation)
                    return
                }
                action = nil
                guard finishMutation(checkpoint) else { return }
                banner = ESIMBanner(tone: .error,
                                    message: "删除 Profile 失败",
                                    detail: error.localizedDescription)
            }
        }
    }

    // MARK: - Profile 重命名

    func renameProfile(_ context: ESIMProfileContext, newName: String) {
        guard capabilities?.canRename == true, !isBusy else { return }
        guard let checkpoint = beginMutation("rename") else { return }
        let generation = sessionGeneration
        beginProfileAction(.renaming(context.id))
        actionTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.sessionGeneration {
                    self.actionTask = nil
                }
            }
            do {
                let result: MessageResponse = try await APIClient(timeoutInterval: 30).send(
                    "\(apiPrefix)/profile", method: "PATCH",
                    body: ESIMRenameRequest(
                        iccid: context.profile.iccid,
                        aid: context.aidHex,
                        name: newName,
                        clientRequestID: checkpoint.clientRequestID))
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                action = nil
                guard finishMutation(checkpoint) else { return }
                if let warning = result.warning, !warning.isEmpty {
                    banner = ESIMBanner(tone: .warning, message: "Profile 名称已修改", detail: warning)
                } else {
                    banner = ESIMBanner(tone: .success, message: "Profile 名称修改成功", detail: nil)
                }
                load(forceRefresh: true)
            } catch {
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                if operationOutcomeIsUnknown(error) {
                    await recoverMutation(checkpoint, title: "修改名称", generation: generation)
                    return
                }
                action = nil
                guard finishMutation(checkpoint) else { return }
                banner = ESIMBanner(tone: .error,
                                    message: "修改名称失败",
                                    detail: error.localizedDescription)
            }
        }
    }

    // MARK: - Profile 下载

    func startDownload(_ request: ESIMDownloadRequest) {
        guard capabilities?.canDownload == true, !isBusy else { return }
        guard let checkpoint = beginMutation("download") else { return }
        let generation = sessionGeneration
        beginProfileAction(.downloading)
        actionTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == self.sessionGeneration { self.actionTask = nil }
            }
            do {
                let started: ESIMOperationSnapshot = try await APIClient().send(
                    "\(apiPrefix)/download", body: request.identified(by: checkpoint.clientRequestID))
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                guard started.kind == "download_profile", !started.id.isEmpty else {
                    banner = ESIMBanner(
                        tone: .warning,
                        message: "下载 Profile 结果待确认",
                        detail: "服务未返回有效操作编号；写卡操作保持锁定")
                    return
                }
                pendingDownloadOperationID = started.id
                trackOperation(started)
            } catch {
                guard !Task.isCancelled, generation == sessionGeneration else { return }
                if operationOutcomeIsUnknown(error) {
                    await recoverMutation(checkpoint, title: "下载 Profile", generation: generation)
                    return
                }
                action = nil
                guard finishMutation(checkpoint) else { return }
                banner = ESIMBanner(tone: .error,
                                    message: "无法开始下载 Profile",
                                    detail: error.localizedDescription)
            }
        }
    }

    func confirmDownload(operationID: String, confirm: Bool) {
        guard !hasCorruptMutationCheckpoint() else { return }
        guard operation?.id == operationID, operation?.awaitsDownloadConfirmation == true else { return }
        let generation = sessionGeneration
        Task { [weak self] in
            guard let self else { return }
            do {
                let updated: ESIMOperationSnapshot = try await APIClient(timeoutInterval: 10).send(
                    "\(apiPrefix)/download/confirm",
                    body: ESIMDownloadConfirmationRequest(operationID: operationID, confirm: confirm))
                guard generation == sessionGeneration, updated.id == operationID else { return }
                operation = updated
            } catch {
                guard generation == sessionGeneration else { return }
                banner = ESIMBanner(tone: .error, message: "无法提交下载确认", detail: error.localizedDescription)
            }
        }
    }

    private func hasCorruptMutationCheckpoint() -> Bool {
        if case .corrupt(let diagnostic) = mutationCheckpoints.load(scope: "reader") {
            corruptMutationCheckpoint = diagnostic
        }
        guard corruptMutationCheckpoint != nil else { return false }
        showCorruptMutationCheckpoint()
        return true
    }

    private func showCorruptMutationCheckpoint() {
        action = .downloading
        banner = ESIMBanner(
            tone: .error, message: "本地 eSIM 请求记录损坏；写卡操作保持锁定",
            detail: "请保留现场并检查诊断；不会自动清除或覆盖。\(corruptMutationCheckpoint ?? "")")
    }

    private func beginMutation(_ actionName: String) -> ESIMMutationCheckpoint? {
        guard !hasCorruptMutationCheckpoint() else { return nil }
        guard pendingMutation == nil,
              let checkpoint = mutationCheckpoints.begin(scope: "reader", action: actionName),
              checkpoint.action == actionName else {
            action = .downloading
            banner = ESIMBanner(
                tone: .warning,
                message: "已有 eSIM 操作结果待确认",
                detail: "请先恢复原 Rust 请求回执；系统不会生成第二个写卡请求编号")
            return nil
        }
        pendingMutation = checkpoint
        return checkpoint
    }

    private func finishMutation(_ checkpoint: ESIMMutationCheckpoint) -> Bool {
        guard mutationCheckpoints.clear(checkpoint) else {
            pendingMutation = checkpoint
            action = .downloading
            banner = ESIMBanner(
                tone: .warning,
                message: "eSIM 回执已返回，但本地请求围栏未能安全清除",
                detail: "写卡操作保持锁定")
            return false
        }
        pendingMutation = nil
        pendingDownloadOperationID = nil
        return true
    }

    private func mutationIntentPath(_ checkpoint: ESIMMutationCheckpoint) -> String {
        let value = checkpoint.clientRequestID.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)
            ?? checkpoint.clientRequestID
        return "\(apiPrefix)/mutation-intent?client_request_id=\(value)"
    }

    private func recoverPendingMutationOnLoad(_ checkpoint: ESIMMutationCheckpoint) {
        guard actionTask == nil else { return }
        let generation = sessionGeneration
        action = .downloading
        actionTask = Task { [weak self] in
            guard let self else { return }
            await self.recoverMutation(checkpoint, title: "eSIM 操作", generation: generation)
        }
    }

    private func recoverMutation(
        _ checkpoint: ESIMMutationCheckpoint,
        title: String,
        generation: UInt64
    ) async {
        do {
            let receipt: ESIMMutationIntentReceipt = try await APIClient(timeoutInterval: 10)
                .get(mutationIntentPath(checkpoint))
            guard generation == sessionGeneration else { return }
            guard receipt.clientRequestID == checkpoint.clientRequestID,
                  receipt.scope == checkpoint.scope,
                  receipt.action == checkpoint.action else {
                actionTask = nil
                action = .downloading
                banner = ESIMBanner(
                    tone: .warning,
                    message: "eSIM 操作结果待确认",
                    detail: "Rust 回执与原请求不一致；写卡操作保持锁定")
                return
            }
            if receipt.isTerminal, receipt.responseStatus == 202 {
                guard let accepted = receipt.acceptedDownloadOperation else {
                    actionTask = nil
                    action = .downloading
                    banner = ESIMBanner(
                        tone: .warning,
                        message: "\(title) 结果待确认",
                        detail: "Rust 受理回执缺少有效下载操作；写卡操作保持锁定")
                    return
                }
                actionTask = nil
                if let pendingDownloadOperationID, pendingDownloadOperationID != accepted.id {
                    action = .downloading
                    banner = ESIMBanner(
                        tone: .warning,
                        message: "下载 Profile 结果待确认",
                        detail: "Rust 回执操作编号变化；写卡操作保持锁定")
                    return
                }
                pendingDownloadOperationID = accepted.id
                banner = ESIMBanner(
                    tone: .info,
                    message: "下载 Profile 已受理，正在确认操作结果",
                    detail: "请求编号：\(checkpoint.clientRequestID)")
                trackOperation(accepted)
                return
            }
            if receipt.isTerminal {
                actionTask = nil
                action = nil
                guard finishMutation(checkpoint) else { return }
                if receipt.succeeded {
                    banner = ESIMBanner(
                        tone: .success,
                        message: "\(title) 已由 Rust 回执确认完成",
                        detail: nil)
                    if pendingMutation == nil { load(forceRefresh: true) }
                } else {
                    banner = ESIMBanner(
                        tone: .error,
                        message: "\(title) 已由 Rust 回执确认未完成",
                        detail: "请求编号：\(checkpoint.clientRequestID)")
                }
            } else {
                actionTask = nil
                action = .downloading
                banner = ESIMBanner(
                    tone: .warning,
                    message: "\(title) 结果待确认",
                    detail: "原请求 \(checkpoint.clientRequestID.suffix(8)) 仍为 \(receipt.phase)；写卡操作保持锁定")
            }
        } catch {
            guard generation == sessionGeneration else { return }
            actionTask = nil
            action = .downloading
            banner = ESIMBanner(
                tone: .warning,
                message: "\(title) 结果待确认",
                detail: "无法读取原 Rust 回执；写卡操作保持锁定")
        }
    }

    // MARK: - 操作跟踪

    func dismissBanner() {
        guard !hasCorruptMutationCheckpoint() else { return }
        banner = nil
    }

    func isProfileActionRunning(_ context: ESIMProfileContext) -> Bool {
        switch action {
        case .switching(let id), .deleting(let id), .renaming(let id): return id == context.id
        default: return false
        }
    }

    func reloadAfterMetadataChange() {
        load()
    }

    // MARK: - 异步操作轮询

    private func trackOperation(_ snapshot: ESIMOperationSnapshot) {
        let generation = sessionGeneration
        operation = snapshot
        if snapshot.kind == "download_profile" && snapshot.isActive {
            action = .downloading
        }
        operationPollTask?.cancel()
        if snapshot.isKnownTerminal {
            handleTerminalOperation(snapshot)
            return
        }
        if snapshot.hasUnknownOutcome {
            banner = ESIMBanner(
                tone: .warning,
                message: "eSIM 操作结果待确认",
                detail: "服务返回了尚未识别的操作状态（\(snapshot.state)）；写卡操作保持锁定")
        }

        operationPollTask = Task { [weak self] in
            guard let self else { return }
            var consecutiveFailures = 0
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                guard !Task.isCancelled else { return }
                do {
                    let response: ESIMOperationResponse = try await APIClient(timeoutInterval: 10).get("\(apiPrefix)/operation")
                    guard !Task.isCancelled, generation == sessionGeneration else { return }
                    consecutiveFailures = 0
                    guard let current = response.operation else {
                        banner = ESIMBanner(
                            tone: .warning,
                            message: "eSIM 操作结果待确认",
                            detail: "服务暂未返回原操作状态；写卡操作仍保持锁定")
                        continue
                    }
                    guard current.id == snapshot.id else {
                        operationPollTask = nil
                        banner = ESIMBanner(
                            tone: .warning,
                            message: "eSIM 操作结果待确认",
                            detail: "服务返回了其他操作编号；原下载写卡操作保持锁定")
                        return
                    }
                    operation = current
                    if current.isKnownTerminal {
                        operationPollTask = nil
                        handleTerminalOperation(current)
                        return
                    } else if current.hasUnknownOutcome {
                        banner = ESIMBanner(
                            tone: .warning,
                            message: "eSIM 操作结果待确认",
                            detail: "服务返回了尚未识别的操作状态（\(current.state)）；写卡操作保持锁定")
                    }
                } catch {
                    guard !Task.isCancelled, generation == sessionGeneration else { return }
                    consecutiveFailures += 1
                    if consecutiveFailures == 8 {
                        banner = ESIMBanner(
                            tone: .warning,
                            message: "仍在等待 eSIM 操作状态",
                            detail: "请稍后刷新卡片确认")
                    }
                    if consecutiveFailures >= 30 {
                        operationPollTask = nil
                        banner = ESIMBanner(
                            tone: .error,
                            message: "eSIM 操作状态读取超时",
                            detail: "已停止后台轮询，但原操作仍保持锁定；请恢复本机服务后确认 Rust 操作终态")
                        return
                    }
                }
            }
        }
    }

    private func handleTerminalOperation(_ snapshot: ESIMOperationSnapshot) {
        guard snapshot.isKnownTerminal else {
            operation = snapshot
            banner = ESIMBanner(
                tone: .warning,
                message: "eSIM 操作结果待确认",
                detail: "未收到 Rust 明确终态；写卡操作保持锁定")
            return
        }
        if let checkpoint = pendingMutation, checkpoint.action == "download" {
            guard pendingDownloadOperationID == snapshot.id else {
                banner = ESIMBanner(
                    tone: .warning,
                    message: "eSIM 操作结果待确认",
                    detail: "终态操作编号与原下载不一致；写卡操作保持锁定")
                return
            }
            guard finishMutation(checkpoint) else { return }
        }
        action = nil
        guard lastHandledOperationID != snapshot.id else { return }
        lastHandledOperationID = snapshot.id

        if snapshot.isFailed {
            banner = ESIMBanner(
                tone: .error,
                message: operationTitle(snapshot.kind, success: false),
                detail: snapshot.error ?? "操作未完成")
            return
        }
        if snapshot.hasWarning {
            banner = ESIMBanner(
                tone: .warning,
                message: snapshot.message ?? operationTitle(snapshot.kind, success: true),
                detail: snapshot.result?.warning)
        } else if snapshot.isCancelled {
            banner = ESIMBanner(tone: .info, message: snapshot.message ?? "已取消下载", detail: nil)
        } else if snapshot.isSuccessful {
            banner = ESIMBanner(
                tone: .success,
                message: snapshot.message ?? operationTitle(snapshot.kind, success: true),
                detail: nil)
        } else {
            banner = ESIMBanner(
                tone: .warning,
                message: "eSIM 操作结果待确认",
                detail: "未收到 Rust 明确成功结果")
            return
        }

        let delay = max(0, snapshot.refreshAfterSeconds ?? 0)
        terminalRefreshTask?.cancel()
        let generation = sessionGeneration
        terminalRefreshTask = Task { [weak self] in
            if delay > 0 {
                try? await Task.sleep(nanoseconds: UInt64(delay) * 1_000_000_000)
            }
            guard let self, !Task.isCancelled, generation == self.sessionGeneration,
                  self.operation?.id == snapshot.id, self.action == nil else { return }
            defer { self.terminalRefreshTask = nil }
            if snapshot.kind == "switch_profile" {
                self.operation = nil
                let alreadySynchronized = self.synchronizedSwitchOperationID == snapshot.id
                if !alreadySynchronized {
                    DashboardStore.shared?.synchronizeReaderAfterProfileSwitch()
                }
                self.load(forceRefresh: alreadySynchronized)
            } else {
                self.load(forceRefresh: true)
            }
        }
    }

    private func publishActiveProfileOperator(from overview: ESIMOverview) {
        let active = (overview.profiles ?? [])
            .flatMap { $0.profiles ?? [] }
            .first(where: { $0.isEnabled })
        if let pending = unconfirmedSwitchICCID {
            let actual = normalizedICCID(DashboardStore.shared?.readerUSIMInfo?.iccid ?? "")
            guard !pending.isEmpty, actual == pending,
                  normalizedICCID(active?.iccid ?? "") == pending else { return }
            unconfirmedSwitchICCID = nil
        }
        let name = active?.serviceProviderName ?? active?.name
        DashboardStore.shared?.updateReaderProfileOperatorName(name)
    }

    private func operationTitle(_ kind: String, success: Bool) -> String {
        switch kind {
        case "download_profile": return success ? "Profile 下载完成" : "Profile 下载失败"
        case "switch_profile": return success ? "Profile 切换完成" : "Profile 切换失败"
        default: return success ? "eSIM 操作完成" : "eSIM 操作失败"
        }
    }
}
