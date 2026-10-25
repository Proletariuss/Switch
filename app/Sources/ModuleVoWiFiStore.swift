import Combine
import Foundation

struct ModuleVoWiFiState: Equatable {
    var revision: UInt64
    var status: VoWiFiStatus?
    var loaded: Bool
    var error: String?
    var actionInFlight: Bool

    static let initial = ModuleVoWiFiState(
        revision: 0,
        status: nil,
        loaded: false,
        error: nil,
        actionInFlight: false)
}

/// Raw user connection fields shared with reader VoWiFi. Swift captures them
/// once per mutation; Rust owns normalization and freezes the resulting intent
/// across all retries and every carrier-specific configuration path.
struct ModuleVoWiFiConnectionIntent: Equatable {
    let proxyHost: String
    let proxyPort: String
    let epdg: String
    let epdgPortText: String

    static let automatic = ModuleVoWiFiConnectionIntent(
        proxyHost: "", proxyPort: "", epdg: "", epdgPortText: "")

    /// The network proxy is a Mac-wide route and can be shared by reader and
    /// module sessions. An ePDG override is card/carrier specific, however, so
    /// the module must not inherit the reader page's global override when the
    /// active SIM changes. Leaving these fields empty makes Rust resolve the
    /// current SIM through the same carrier registry used by reader VoWiFi.
    static func sharedProxyUsingAutomaticCarrierTemplate(
        proxyHost: String,
        proxyPort: String
    ) -> ModuleVoWiFiConnectionIntent {
        ModuleVoWiFiConnectionIntent(
            proxyHost: proxyHost,
            proxyPort: proxyPort,
            epdg: "",
            epdgPortText: "")
    }
}

/// DJI 模块 VoWiFi 的单一状态源。
///
/// 查询、启停及异步结果轮询均受 lifecycle generation 与 operation revision
/// 保护，后端重启或新操作开始后，旧任务不会再覆盖当前状态。
@MainActor
final class ModuleVoWiFiStore: ObservableObject {
    @Published private(set) var state = ModuleVoWiFiState.initial

    private var requestedEnabled: Bool?
    var toggleEnabled: Bool {
        if let requestedEnabled { return requestedEnabled }
        if state.error != nil, state.status?.vowifiActive != true { return false }
        if state.status?.state == "failed" || state.status?.state == "unavailable" { return false }
        return state.status?.imsWiFiCallingEnabled ?? false
    }

    /// Let the user stop an in-progress start request instead of trapping the
    /// switch in a disabled/loading state until the network timeout expires.
    var canCancelStart: Bool {
        state.actionInFlight && requestedEnabled == true && ownerOperation?.requestedEnable == true
    }

    var moduleStatus: () -> DeviceStatus? = { nil }
    var connectionIntent: () -> ModuleVoWiFiConnectionIntent = { .automatic }
    var onToast: (_ message: String, _ isSuccess: Bool, _ title: String?) -> Void = { _, _, _ in }

    private let checkpoints: VoWiFiRequestCheckpointStore
    private var pendingCheckpoint: VoWiFiRequestCheckpoint?
    private var checkpointFailure = false
    private var deferredTerminal: VoWiFiOperation?
    private let backend: BackendProcess
    private var refreshTask: Task<Void, Never>?
    private var operationTask: Task<Void, Never>?
    private var lifecycleGeneration: UInt64 = 0
    private var refreshRevision: UInt64 = 0
    private var operationRevision: UInt64 = 0
    private var ownerOperation: VoWiFiOperation?
    private var observedProfileICCID: String?
    private var consecutiveStatusReadFailures = 0
    private(set) var lastHealthRefreshAt: Date?
    private var cancellables = Set<AnyCancellable>()

    init(backend: BackendProcess, checkpointRoot: URL? = nil) {
        self.checkpoints = VoWiFiRequestCheckpointStore(scope: "module", root: checkpointRoot)
        self.backend = backend
        backend.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] backendState in
                guard let self else { return }
                if case .running = backendState {
                    self.resumeCheckpoint()
                    self.refresh()
                } else {
                    self.reset()
                }
            }
            .store(in: &cancellables)
    }

    deinit {
        refreshTask?.cancel()
        operationTask?.cancel()
    }

    func reset() {
        lifecycleGeneration &+= 1
        refreshRevision &+= 1
        operationRevision &+= 1
        refreshTask?.cancel()
        operationTask?.cancel()
        refreshTask = nil
        operationTask = nil
        requestedEnabled = nil
        ownerOperation = nil
        observedProfileICCID = nil
        consecutiveStatusReadFailures = 0
        lastHealthRefreshAt = nil
        commit(status: nil, loaded: false, error: nil, actionInFlight: pendingCheckpoint != nil)
    }

    /// Binds displayed VoWiFi state to the module's active SIM identity. A
    /// profile transition invalidates late responses and removes the previous
    /// SIM's status before querying the new profile.
    func observeActiveProfileICCID(_ iccid: String?) {
        guard let current = normalizedICCID(iccid), !current.isEmpty else { return }
        guard let previous = observedProfileICCID else {
            observedProfileICCID = current
            return
        }
        guard previous != current else { return }
        observedProfileICCID = current
        invalidateProfileScopedState()
        refresh()
    }

    /// The eSIM manager calls this after a profile switch completes (or the
    /// module reports that the switch may have completed). Do not keep the
    /// former profile's status visible while the dashboard catches up.
    func invalidateAfterProfileSwitch() {
        if let current = normalizedICCID(moduleStatus()?.iccid) {
            observedProfileICCID = current
        }
        invalidateProfileScopedState()
        refresh()
    }

    private func invalidateProfileScopedState() {
        refreshRevision &+= 1
        operationRevision &+= 1
        refreshTask?.cancel()
        operationTask?.cancel()
        refreshTask = nil
        operationTask = nil
        requestedEnabled = nil
        ownerOperation = nil
        consecutiveStatusReadFailures = 0
        commit(status: nil, loaded: false, error: nil, actionInFlight: pendingCheckpoint != nil)
        resumeCheckpoint()
    }

    func refresh() {
        guard case .running = backend.state else { return }
        if observedProfileICCID == nil {
            observedProfileICCID = normalizedICCID(moduleStatus()?.iccid)
        }
        if let hardwareStatus = moduleStatus()?.hardwareStatus, !hardwareStatus.isEmpty {
            refreshRevision &+= 1
            refreshTask?.cancel()
            refreshTask = nil
            requestedEnabled = nil
            commit(status: nil, loaded: true, error: nil, actionInFlight: state.actionInFlight)
            return
        }
        guard refreshTask == nil else { return }

        refreshRevision &+= 1
        let expectedRefresh = refreshRevision
        let expectedGeneration = lifecycleGeneration
        lastHealthRefreshAt = Date()
        refreshTask = Task { [weak self] in
            do {
                let latest: VoWiFiStatus = try await APIClient(timeoutInterval: 12)
                    .get("api/vowifi")
                guard let self,
                      self.isCurrentRefresh(expectedRefresh, generation: expectedGeneration) else { return }
                self.refreshTask = nil
                self.consecutiveStatusReadFailures = 0
                if !self.state.actionInFlight {
                    self.requestedEnabled = nil
                }
                let profileBoundStatus = self.statusBelongsToActiveProfile(latest) ? latest : nil
                self.commit(
                    status: profileBoundStatus,
                    loaded: true,
                    error: nil,
                    actionInFlight: self.state.actionInFlight)
            } catch {
                guard let self,
                      self.isCurrentRefresh(expectedRefresh, generation: expectedGeneration) else { return }
                self.refreshTask = nil
                let message = error.localizedDescription
                if isModuleDisconnectedError(message) {
                    self.requestedEnabled = nil
                    self.consecutiveStatusReadFailures = 0
                } else {
                    self.consecutiveStatusReadFailures += 1
                }
                self.commit(
                    status: isModuleDisconnectedError(message) || self.consecutiveStatusReadFailures >= 2
                        ? nil : self.state.status,
                    loaded: true,
                    error: isModuleDisconnectedError(message) ? nil : "读取 VoWiFi 状态失败：\(message)",
                    actionInFlight: self.state.actionInFlight)
            }
        }
    }

    func enable() {
        setEnabled(true)
    }

    func disable() {
        setEnabled(false)
    }

    private func setEnabled(_ enabled: Bool) {
        guard case .running = backend.state else { return }
        if state.actionInFlight {
            // A pending start is cancellable; duplicate starts and attempts to
            // reverse an already-running stop remain ignored.
            guard !enabled, requestedEnabled == true, ownerOperation?.requestedEnable == true else { return }
        }
        // An earlier status read describes the pre-operation state. Invalidate it
        // before sending the mutation so it cannot overwrite the final result.
        refreshRevision &+= 1
        refreshTask?.cancel()
        refreshTask = nil
        operationRevision &+= 1
        let expectedOperation = operationRevision
        let expectedGeneration = lifecycleGeneration
        requestedEnabled = enabled
        let clientRequestID = UUID().uuidString
        let connection = enabled ? connectionIntent() : .automatic
        let request = VoWiFiRequest(
            enable: enabled,
            wifiCallingMode: enabled ? 1 : nil,
            proxyHost: enabled ? connection.proxyHost : nil,
            proxyPort: enabled ? connection.proxyPort : nil,
            epdg: enabled ? connection.epdg : nil,
            epdgPortText: enabled ? connection.epdgPortText : nil,
            observeOperation: true,
            operationID: enabled ? nil : ownerOperation?.operationID,
            operationGeneration: enabled ? nil : ownerOperation?.generation,
            clientRequestID: clientRequestID)

        do {
            pendingCheckpoint = try checkpoints.begin(id: clientRequestID, enable: enabled, request: request)
        } catch {
            markOperationObservationPending("无法持久化 VoWiFi 请求：\(error.localizedDescription)；操作保持锁定")
            return
        }
        operationTask?.cancel()
        commit(status: state.status, loaded: state.loaded, error: nil, actionInFlight: true)
        operationTask = Task { [weak self] in
            guard let self else { return }
            do {
                let accepted: VoWiFiAccepted = try await APIClient(timeoutInterval: 10)
                    .send("api/vowifi", body: request)
                guard self.isCurrentOperation(expectedOperation, generation: expectedGeneration) else { return }
                guard let owner = accepted.operation,
                      owner.clientRequestID == clientRequestID, owner.requestedEnable == enabled else {
                    self.markOperationObservationPending(
                        "操作已被接受，但结构化回执暂不可见；正在从 Rust owner 恢复")
                    await self.recoverOperationAfterUnknownMutation(
                        operation: expectedOperation,
                        generation: expectedGeneration,
                        requestedEnable: enabled,
                        clientRequestID: clientRequestID)
                    return
                }
                self.ownerOperation = owner
                try await self.pollResult(
                    operation: expectedOperation,
                    generation: expectedGeneration,
                    owner: owner)
            } catch is CancellationError {
                return
            } catch {
                guard self.isCurrentOperation(expectedOperation, generation: expectedGeneration) else { return }
                if mutationFailureRequiresStatusReconciliation(error) {
                    self.markOperationObservationPending(
                        "操作请求已发出，但回执暂不可见；正在从 Rust owner 恢复")
                    await self.recoverOperationAfterUnknownMutation(
                        operation: expectedOperation,
                        generation: expectedGeneration,
                        requestedEnable: enabled,
                        clientRequestID: clientRequestID)
                    return
                }
                self.markOperationObservationPending("VoWiFi 请求结果待确认：\(error.localizedDescription)")
                await self.recoverOperationAfterUnknownMutation(
                    operation: expectedOperation, generation: expectedGeneration,
                    requestedEnable: enabled, clientRequestID: clientRequestID)
            }
        }
    }

    private func resumeCheckpoint() {
        guard operationTask == nil else { return }
        do {
            pendingCheckpoint = try checkpoints.load()
            checkpointFailure = false
        }
        catch {
            checkpointFailure = true
            markOperationObservationPending("VoWiFi 请求检查点不可读；操作保持锁定")
            return
        }
        guard let checkpoint = pendingCheckpoint else { return }
        requestedEnabled = checkpoint.requestedEnable
        operationRevision &+= 1
        let operation = operationRevision
        let generation = lifecycleGeneration
        markOperationObservationPending("正在恢复原 VoWiFi 请求回执")
        operationTask = Task { [weak self] in
            guard let self else { return }
            await self.recoverOperationAfterUnknownMutation(
                operation: operation, generation: generation,
                requestedEnable: checkpoint.requestedEnable, clientRequestID: checkpoint.clientRequestID)
        }
    }

    private func recoverOperationAfterUnknownMutation(
        operation: UInt64,
        generation: UInt64,
        requestedEnable: Bool,
        clientRequestID: String
    ) async {
        while isCurrentOperation(operation, generation: generation) {
            do {
                let result: VoWiFiResult = try await APIClient(timeoutInterval: 10)
                    .get("api/vowifi/result?\(VoWiFiOperation.requestObservationQuery(clientRequestID: clientRequestID))")
                guard isCurrentOperation(operation, generation: generation) else { return }
                if let owner = result.operation,
                   owner.clientRequestID == clientRequestID,
                   owner.requestedEnable == requestedEnable {
                    ownerOperation = owner
                    do {
                        try await pollResult(operation: operation, generation: generation, owner: owner)
                    } catch is CancellationError {}
                    return
                }
            } catch is CancellationError {
                return
            } catch {
                guard isCurrentOperation(operation, generation: generation) else { return }
                if (error as? APIError)?.code == "vowifi_request_id_unknown" {
                    markOperationObservationPending(
                        "原请求回执未找到；提交结果待安全核对，已保留请求检查点")
                    do {
                        // Upload the original checkpoint bytes to a read-only
                        // reconciliation boundary, never to the mutation route.
                        guard let checkpoint = try checkpoints.load(),
                              checkpoint.clientRequestID == clientRequestID,
                              checkpoint.requestedEnable == requestedEnable else {
                            markOperationObservationPending("原请求检查点不匹配；操作保持锁定，等待安全核对")
                            try await Task.sleep(for: .milliseconds(1_500))
                            continue
                        }
                        let reconciled: VoWiFiAccepted = try await APIClient(timeoutInterval: 10)
                            .upload("api/vowifi/reconcile", data: checkpoint.payload)
                        guard isCurrentOperation(operation, generation: generation) else { return }
                        guard let owner = reconciled.operation,
                              owner.clientRequestID == clientRequestID,
                              owner.requestedEnable == requestedEnable else {
                            markOperationObservationPending("安全核对回执不匹配；操作保持锁定")
                            try await Task.sleep(for: .milliseconds(1_500))
                            continue
                        }
                        ownerOperation = owner
                        try await pollResult(operation: operation, generation: generation, owner: owner)
                        return
                    } catch is CancellationError {
                        return
                    } catch {
                        guard isCurrentOperation(operation, generation: generation) else { return }
                        if (error as? APIError)?.code == "vowifi_reconcile_owner_unconfirmed" {
                            markOperationObservationPending(
                                "原请求回执未找到；后台资源仍待安全核对，已保留请求检查点")
                        } else {
                            markOperationObservationPending(
                                "原请求回执未找到；安全核对尚未完成：\(error.localizedDescription)；已保留请求检查点")
                        }
                    }
                } else {
                    markOperationObservationPending(
                        "操作回执仍不可见：\(error.localizedDescription)；后台 owner 尚未确认结束")
                }
            }
            do {
                try await Task.sleep(for: .milliseconds(1_500))
            } catch {
                return
            }
        }
    }

    private func pollResult(operation: UInt64, generation: UInt64, owner: VoWiFiOperation) async throws {
        var projection = owner
        while true {
            try Task.checkCancellation()
            guard isCurrentOperation(operation, generation: generation),
                  ownerOperation?.matches(owner) == true else { throw CancellationError() }
            if projection.terminal {
                do {
                    try checkpoints.clear(terminal: projection)
                    pendingCheckpoint = try checkpoints.load()
                    checkpointFailure = false
                } catch {
                    markOperationObservationPending("保存终结回执失败：\(error.localizedDescription)")
                    return
                }
                if pendingCheckpoint != nil {
                    deferredTerminal = deferredTerminal ?? projection
                    operationTask = nil
                    resumeCheckpoint()
                    return
                }
                let completion = deferredTerminal ?? projection
                deferredTerminal = nil
                if completion.succeeded {
                    operationTask = nil
                    requestedEnabled = nil
                    commit(status: state.status, loaded: true, error: nil, actionInFlight: false)
                    onToast(completion.message, true, "Wi-Fi Calling")
                    refresh()
                } else {
                    finishOperationFailure(completion.message, status: state.status)
                }
                return
            }
            if projection.observationExpired {
                markOperationObservationPending(projection.message)
            }
            // Observation expiry only ends the server-side long poll. The Rust
            // owner and its RF/network claim remain alive, so keep the same
            // identity fenced and re-observe it at a bounded cadence.
            let delay = max(projection.pollAfterMS, projection.observationExpired ? 1_500 : 1)
            try await Task.sleep(for: .milliseconds(delay))
            let result: VoWiFiResult
            do {
                result = try await APIClient(timeoutInterval: 10)
                    .get("api/vowifi/result?\(owner.observationQuery)")
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                markOperationObservationPending(
                    "操作观察中断：\(error.localizedDescription)；后台 owner 尚未确认结束")
                projection = VoWiFiOperation.pendingObservation(from: projection)
                continue
            }
            guard isCurrentOperation(operation, generation: generation),
                  ownerOperation?.matches(owner) == true else { throw CancellationError() }
            guard let observed = result.operation, observed.matches(owner),
                  observed.clientRequestID == owner.clientRequestID,
                  observed.requestedEnable == owner.requestedEnable else {
                markOperationObservationPending("操作代次不匹配或后端暂时缺少结构化状态；后台 owner 尚未确认结束")
                projection = VoWiFiOperation.pendingObservation(from: projection)
                continue
            }
            projection = observed
            // A terminal, expired or superseded receipt must be evaluated
            // before any companion status can republish readiness.
            if projection.terminal || projection.observationExpired {
                continue
            }
            if let status = result.status, statusBelongsToActiveProfile(status) {
                commit(status: status, loaded: true, error: nil, actionInFlight: true)
            }
        }
    }

    private func markOperationObservationPending(_ message: String) {
        let detail = message.trimmingCharacters(in: .whitespacesAndNewlines)
        let displayMessage = detail.isEmpty
            ? "操作结果待确认；后台 owner 仍在工作"
            : detail
        commit(status: state.status, loaded: true, error: displayMessage, actionInFlight: true)
    }

    private func finishOperationFailure(_ message: String, status: VoWiFiStatus?) {
        operationTask = nil
        requestedEnabled = nil
        let detail = message.trimmingCharacters(in: .whitespacesAndNewlines)
        let displayMessage = detail.isEmpty ? "模块 Wi-Fi Calling 连接失败" : detail
        let currentStatus = status?.progressStep == 0 ? state.status : status
        commit(status: currentStatus, loaded: true, error: displayMessage, actionInFlight: false)
        onToast(displayMessage, false, "Wi-Fi Calling")
        // Reconcile display state only after the Rust receipt is terminal.
        refresh()
    }

    private func isCurrentRefresh(_ revision: UInt64, generation: UInt64) -> Bool {
        !Task.isCancelled &&
            lifecycleGeneration == generation &&
            refreshRevision == revision
    }

    private func isCurrentOperation(_ revision: UInt64, generation: UInt64) -> Bool {
        !Task.isCancelled &&
            lifecycleGeneration == generation &&
            operationRevision == revision
    }

    private func statusBelongsToActiveProfile(_ status: VoWiFiStatus) -> Bool {
        guard let active = normalizedICCID(moduleStatus()?.iccid),
              let bound = normalizedICCID(status.simICCID) else { return true }
        return active == bound
    }

    private func normalizedICCID(_ value: String?) -> String? {
        guard let value else { return nil }
        let digits = value.filter(\.isNumber)
        return digits.isEmpty ? nil : digits
    }

    private func commit(
        status: VoWiFiStatus?,
        loaded: Bool,
        error: String?,
        actionInFlight: Bool
    ) {
        state = ModuleVoWiFiState(
            revision: state.revision &+ 1,
            status: status,
            loaded: loaded,
            error: error,
            actionInFlight: actionInFlight || pendingCheckpoint != nil || checkpointFailure)
    }

    private struct EmptyResponse: Decodable {}
}
