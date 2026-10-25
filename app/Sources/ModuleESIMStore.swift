import Combine
import Foundation

@MainActor
final class ModuleESIMStore: ObservableObject {
    @Published private(set) var overview: ESIMOverview?
    @Published private(set) var profileNotes: [String: ProfileNote] = [:]
    @Published private(set) var operation: ESIMOperationSnapshot?
    @Published private(set) var busy = false
    @Published private(set) var error: String?
    @Published private(set) var message: String?
    @Published private(set) var refreshOutcome: ESIMRefreshOutcome?

    private var generation: UInt64 = 0
    private var task: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?
    private var notesTask: Task<Void, Never>?
    private let mutationCheckpoints: ESIMMutationCheckpointStore
    private var pendingMutation: ESIMMutationCheckpoint?
    private var pendingDownloadOperationID: String?
    private var corruptMutationCheckpoint: String?

    init(mutationCheckpoints: ESIMMutationCheckpointStore = ESIMMutationCheckpointStore()) {
        self.mutationCheckpoints = mutationCheckpoints
        switch mutationCheckpoints.load(scope: "module") {
        case .missing: break
        case .valid(let checkpoint): pendingMutation = checkpoint
        case .corrupt(let diagnostic): corruptMutationCheckpoint = diagnostic
        }
        if corruptMutationCheckpoint != nil { showCorruptMutationCheckpoint() }
        if pendingMutation != nil {
            busy = true
            error = "eSIM 操作结果待确认：正在按原请求编号恢复 Rust 回执"
        }
    }

    deinit {
        task?.cancel()
        pollTask?.cancel()
        notesTask?.cancel()
    }

    var profiles: [ESIMProfileContext] {
        (overview?.profiles ?? []).flatMap { group in
            (group.profiles ?? []).map {
                ESIMProfileContext(eid: group.eid ?? "", aidHex: group.aidHex ?? "", profile: $0)
            }
        }
    }

    func savedPhoneNumber(for iccid: String) -> String? {
        let target = normalizedICCID(iccid)
        guard !target.isEmpty else { return nil }
        return usableSIMPhoneNumber(profileNotes.first(where: { normalizedICCID($0.key) == target })?.value.phone)
    }

    private func normalizedICCID(_ value: String) -> String {
        value.filter(\.isNumber)
    }

    func clear() {
        generation &+= 1
        task?.cancel()
        pollTask?.cancel()
        task = nil
        pollTask = nil
        notesTask?.cancel()
        notesTask = nil
        overview = nil
        refreshOutcome = nil
        profileNotes = [:]
        operation = nil
        pendingDownloadOperationID = nil
        busy = pendingMutation != nil || corruptMutationCheckpoint != nil
        error = nil
        message = nil
        if corruptMutationCheckpoint != nil { showCorruptMutationCheckpoint() }
    }

    func load(refresh: Bool = false) {
        guard !hasCorruptMutationCheckpoint() else { return }
        if let pendingMutation {
            recoverPendingMutationOnLoad(pendingMutation)
            return
        }
        guard !busy else { return }
        generation &+= 1
        let current = generation
        task?.cancel()
        busy = true
        error = nil
        if refresh {
            refreshOutcome = nil
            message = nil
        }
        task = Task { [weak self] in
            guard let self else { return }
            defer {
                if self.generation == current {
                    self.busy = false
                    self.task = nil
                }
            }
            do {
                let path = refresh ? "api/esim?refresh=true" : "api/esim"
                let result: ESIMOverview = try await APIClient(timeoutInterval: refresh ? 145 : 45).get(path)
                guard !Task.isCancelled, self.generation == current else { return }
                self.overview = result
                self.refreshOutcome = result.refresh
                if let outcome = result.refresh {
                    if outcome.completed {
                        self.message = outcome.message
                    } else {
                        self.message = nil
                        self.error = outcome.message
                    }
                }
                let retainedID = self.pendingDownloadOperationID ??
                    (self.operation?.blocksNewMutation == true ? self.operation?.id : nil)
                if let retainedID, result.operation?.id != retainedID {
                    self.error = "eSIM 操作结果待确认：概览未返回原操作编号；写卡操作保持锁定"
                } else if self.pendingMutation?.action == "download", retainedID == nil {
                    self.error = "eSIM 操作结果待确认：须按原请求编号恢复下载回执；写卡操作保持锁定"
                } else if let observed = result.operation {
                    self.operation = observed
                    if observed.hasUnknownOutcome {
                        self.error = "eSIM 操作结果待确认：服务返回未知状态（\(observed.state)），写卡操作保持锁定"
                    }
                    if observed.blocksNewMutation { self.pollOperation(generation: current) }
                } else if self.operation?.blocksNewMutation == true {
                    self.error = "eSIM 操作结果待确认：服务暂未返回原操作状态，写卡操作仍保持锁定"
                } else {
                    self.operation = nil
                }
                self.loadProfileNotes(generation: current)
            } catch {
                guard !Task.isCancelled, self.generation == current else { return }
                self.refreshOutcome = (error as? any ESIMRefreshFailure)?.refreshOutcome
                self.message = nil
                self.error = error.localizedDescription
            }
        }
    }

    private func loadProfileNotes(generation current: UInt64) {
        notesTask?.cancel()
        notesTask = Task { [weak self] in
            guard let self else { return }
            let result: NotesResponse? = try? await APIClient(timeoutInterval: 8).get("api/esim/notes")
            guard !Task.isCancelled, self.generation == current else { return }
            self.profileNotes = result?.notes ?? [:]
            self.notesTask = nil
        }
    }

    func switchProfile(_ item: ESIMProfileContext) {
        guard !busy, operation?.blocksNewMutation != true,
              overview?.capabilities?.canSwitch == true, !item.profile.isEnabled else { return }
        guard let checkpoint = beginMutation("switch") else { return }
        perform("切换 Profile", checkpoint: checkpoint) {
            let result: ESIMSwitchResult = try await APIClient(timeoutInterval: 130).send(
                "api/esim/switch", body: ESIMSwitchRequest(
                    iccid: item.profile.iccid,
                    aid: item.aidHex,
                    clientRequestID: checkpoint.clientRequestID))
            return result.operation
        }
    }

    func deleteProfile(_ item: ESIMProfileContext) {
        guard !busy, operation?.blocksNewMutation != true,
              overview?.capabilities?.canDelete == true, !item.profile.isEnabled else { return }
        guard let checkpoint = beginMutation("delete") else { return }
        perform("删除 Profile", checkpoint: checkpoint) {
            let _: ESIMProfileResult = try await APIClient(timeoutInterval: 90).send(
                "api/esim/profile", method: "DELETE",
                body: ESIMDeleteRequest(
                    iccid: item.profile.iccid,
                    aid: item.aidHex,
                    clientRequestID: checkpoint.clientRequestID))
            return nil
        }
    }

    func download(_ request: ESIMDownloadRequest) {
        guard !busy, operation?.blocksNewMutation != true,
              overview?.capabilities?.canDownload == true else { return }
        guard let checkpoint = beginMutation("download") else { return }
        perform("下载 Profile", checkpoint: checkpoint) {
            let started: ESIMOperationSnapshot = try await APIClient(timeoutInterval: 45)
                .send("api/esim/download", body: request.identified(by: checkpoint.clientRequestID))
            return started
        }
    }

    func confirmDownload(operationID: String, confirm: Bool) {
        guard !hasCorruptMutationCheckpoint() else { return }
        guard operation?.id == operationID, operation?.awaitsDownloadConfirmation == true else { return }
        let current = generation
        Task { [weak self] in
            guard let self else { return }
            do {
                let updated: ESIMOperationSnapshot = try await APIClient(timeoutInterval: 10).send(
                    "api/esim/download/confirm",
                    body: ESIMDownloadConfirmationRequest(operationID: operationID, confirm: confirm))
                guard generation == current, updated.id == operationID else { return }
                operation = updated
            } catch {
                guard generation == current else { return }
                self.error = "无法提交下载确认：\(error.localizedDescription)"
            }
        }
    }

    private func perform(
        _ title: String,
        checkpoint: ESIMMutationCheckpoint,
        action: @escaping () async throws -> ESIMOperationSnapshot?
    ) {
        generation &+= 1
        let current = generation
        task?.cancel()
        pollTask?.cancel()
        busy = true
        error = nil
        message = nil
        task = Task { [weak self] in
            guard let self else { return }
            do {
                let result = try await action()
                guard !Task.isCancelled, self.generation == current else { return }
                self.busy = false
                self.task = nil
                if checkpoint.action == "download" {
                    guard let result, result.kind == "download_profile", !result.id.isEmpty else {
                        self.busy = true
                        self.error = "下载 Profile 结果待确认：服务未返回有效操作编号；写卡操作保持锁定"
                        return
                    }
                    self.pendingDownloadOperationID = result.id
                    if result.isKnownTerminal {
                        guard self.finishMutation(checkpoint) else { return }
                    }
                } else {
                    guard self.finishMutation(checkpoint) else { return }
                }
                self.operation = result
                if result?.blocksNewMutation == true {
                    self.pollOperation(generation: current)
                } else if result == nil {
                    self.message = "\(title)已提交"
                    self.load(refresh: true)
                } else if result?.isSuccessful == true {
                    self.message = result?.message ?? "\(title)已完成"
                    self.load(refresh: true)
                } else if result?.hasWarning == true {
                    self.error = result?.result?.warning ?? result?.message ?? "\(title)完成，但需要核对卡片状态"
                    self.load(refresh: true)
                } else if result?.isFailed == true || result?.isCancelled == true {
                    self.error = result?.error ?? result?.message ?? "\(title)未完成"
                } else {
                    self.error = "\(title)结果待确认：服务未返回明确终态，写卡操作保持锁定"
                }
            } catch {
                guard !Task.isCancelled, self.generation == current else { return }
                if operationOutcomeIsUnknown(error) {
                    self.task = nil
                    self.busy = true
                    await self.recoverMutation(checkpoint, title: title, generation: current)
                    return
                }
                self.busy = false
                self.task = nil
                guard self.finishMutation(checkpoint) else { return }
                self.error = "\(title)失败：\(error.localizedDescription)"
            }
        }
    }

    private func hasCorruptMutationCheckpoint() -> Bool {
        if case .corrupt(let diagnostic) = mutationCheckpoints.load(scope: "module") {
            corruptMutationCheckpoint = diagnostic
        }
        guard corruptMutationCheckpoint != nil else { return false }
        showCorruptMutationCheckpoint()
        return true
    }

    private func showCorruptMutationCheckpoint() {
        busy = true
        error = "本地 eSIM 请求记录损坏；写卡操作保持锁定。请保留现场并检查诊断；不会自动清除或覆盖。\(corruptMutationCheckpoint ?? "")"
    }

    private func beginMutation(_ action: String) -> ESIMMutationCheckpoint? {
        guard !hasCorruptMutationCheckpoint() else { return nil }
        guard pendingMutation == nil,
              let checkpoint = mutationCheckpoints.begin(scope: "module", action: action),
              checkpoint.action == action else {
            busy = true
            error = "已有 eSIM 操作结果待确认，请先恢复原请求回执"
            return nil
        }
        pendingMutation = checkpoint
        return checkpoint
    }

    private func finishMutation(_ checkpoint: ESIMMutationCheckpoint) -> Bool {
        guard mutationCheckpoints.clear(checkpoint) else {
            pendingMutation = checkpoint
            busy = true
            error = "eSIM 回执已返回，但本地请求围栏未能安全清除"
            return false
        }
        pendingMutation = nil
        pendingDownloadOperationID = nil
        return true
    }

    private func mutationIntentPath(_ checkpoint: ESIMMutationCheckpoint) -> String {
        let value = checkpoint.clientRequestID.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)
            ?? checkpoint.clientRequestID
        return "api/esim/mutation-intent?client_request_id=\(value)"
    }

    private func recoverPendingMutationOnLoad(_ checkpoint: ESIMMutationCheckpoint) {
        guard task == nil else { return }
        generation &+= 1
        let current = generation
        busy = true
        task = Task { [weak self] in
            guard let self else { return }
            await self.recoverMutation(checkpoint, title: "eSIM 操作", generation: current)
        }
    }

    private func recoverMutation(
        _ checkpoint: ESIMMutationCheckpoint,
        title: String,
        generation current: UInt64
    ) async {
        do {
            let receipt: ESIMMutationIntentReceipt = try await APIClient(timeoutInterval: 10)
                .get(mutationIntentPath(checkpoint))
            guard generation == current else { return }
            guard receipt.clientRequestID == checkpoint.clientRequestID,
                  receipt.scope == checkpoint.scope,
                  receipt.action == checkpoint.action else {
                task = nil
                busy = true
                error = "eSIM 操作结果待确认：Rust 回执与原请求不一致；写卡操作保持锁定"
                return
            }
            if receipt.isTerminal, receipt.responseStatus == 202 {
                guard let accepted = receipt.acceptedDownloadOperation else {
                    task = nil
                    busy = true
                    error = "\(title) 结果待确认：Rust 受理回执缺少有效下载操作；写卡操作保持锁定"
                    return
                }
                task = nil
                busy = false
                if let pendingDownloadOperationID, pendingDownloadOperationID != accepted.id {
                    busy = true
                    error = "下载 Profile 结果待确认：Rust 回执操作编号变化；写卡操作保持锁定"
                    return
                }
                pendingDownloadOperationID = accepted.id
                error = nil
                message = nil
                operation = accepted
                pollOperation(generation: current)
                return
            }
            if receipt.isTerminal {
                task = nil
                busy = false
                guard finishMutation(checkpoint) else { return }
                if receipt.succeeded {
                    message = "\(title) 已由 Rust 回执确认完成"
                    if pendingMutation == nil { load(refresh: true) }
                } else {
                    error = "\(title) 已由 Rust 回执确认未完成"
                }
            } else {
                task = nil
                busy = true
                error = "\(title) 结果待确认（请求 \(checkpoint.clientRequestID.suffix(8))）；写卡操作保持锁定"
            }
        } catch {
            guard generation == current else { return }
            task = nil
            busy = true
            self.error = "\(title) 结果待确认：无法读取原 Rust 回执；写卡操作保持锁定"
        }
    }

    private func pollOperation(generation current: UInt64) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            guard let self else { return }
            // A module download can run for the backend's full five-minute
            // transfer window. Keep polling long enough to receive its real
            // terminal error instead of showing a false timeout at 3 minutes.
            for _ in 0..<180 {
                try? await Task.sleep(for: .seconds(2))
                guard !Task.isCancelled, self.generation == current else { return }
                do {
                    let result: ESIMOperationResponse = try await APIClient(timeoutInterval: 15)
                        .get("api/esim/operation")
                    guard !Task.isCancelled, self.generation == current else { return }
                    guard let observed = result.operation else {
                        self.error = "eSIM 操作结果待确认：服务暂未返回原操作状态，写卡操作仍保持锁定"
                        continue
                    }
                    guard observed.id == (self.pendingDownloadOperationID ?? self.operation?.id) else {
                        self.pollTask = nil
                        self.error = "eSIM 操作结果待确认：服务返回了其他操作编号；原下载写卡操作保持锁定"
                        return
                    }
                    self.operation = observed
                    if observed.isKnownTerminal {
                        self.pollTask = nil
                        if let checkpoint = self.pendingMutation, checkpoint.action == "download" {
                            guard self.pendingDownloadOperationID == observed.id,
                                  self.finishMutation(checkpoint) else { return }
                        }
                        if observed.isFailed {
                            self.error = observed.error ?? "eSIM 操作失败"
                        } else if observed.isSuccessful {
                            self.message = observed.message ?? "eSIM 操作已完成"
                            self.load(refresh: true)
                        } else if observed.hasWarning {
                            self.error = observed.result?.warning ?? observed.message ?? "eSIM 操作完成，但需要核对卡片状态"
                            self.load(refresh: true)
                        } else if observed.isCancelled {
                            self.error = observed.message ?? "eSIM 操作已取消"
                        }
                        return
                    } else if observed.hasUnknownOutcome {
                        self.error = "eSIM 操作结果待确认：服务返回未知状态（\(observed.state)），写卡操作保持锁定"
                    }
                } catch {
                    // Module restart can briefly interrupt the service socket.
                }
            }
            guard !Task.isCancelled, self.generation == current else { return }
            self.pollTask = nil
            self.error = "eSIM 操作状态读取超时；原操作仍保持锁定，请恢复服务并确认 Rust 操作终态"
        }
    }
}
