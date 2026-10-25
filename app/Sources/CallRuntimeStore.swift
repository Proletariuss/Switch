import Combine
import Foundation

protocol CallCheckpointDefaults: AnyObject {
    func object(forKey defaultName: String) -> Any?
    func set(_ value: Any?, forKey defaultName: String)
    func removeObject(forKey defaultName: String)
    func synchronize() -> Bool
}

extension UserDefaults: CallCheckpointDefaults {}

/// Owns the call-control state shared by the module and reader VoWiFi paths.
/// Media routing stays in `DashboardStore`; this store serializes signalling,
/// endpoint selection and DTMF so late replies cannot revive an ended call.
@MainActor
final class CallRuntimeStore: ObservableObject {
    @Published var status = CallStatus(state: "idle", number: nil, incoming: false, active: false)
    @Published var answerInFlight = false
    @Published private(set) var dialInFlight = false
    @Published private(set) var rejectInFlight = false
    @Published private(set) var hangupInFlight = false
    var commandInFlight: Bool {
        dialInFlight || answerInFlight || rejectInFlight || hangupInFlight
            || pendingMutation != nil || corruptPendingCheckpoint
    }
    @Published var endpoint = "module"
    @Published var readerState: ReaderVoWiFiCallState?
    @Published var dialNumber = ""
    @Published var error: String?

    var readerAvailable: (() -> Bool)?
    var readerIMSRegistered: (() -> Bool)?
    var moduleVoWiFiActive: (() -> Bool)?
    var moduleCallModeReady: (() -> Bool)?
    var onStatusChange: ((CallStatus, CallStatus) -> Void)?
    var onPrepareHangup: ((String) -> Void)?
    var onModuleHangupWarning: ((String) -> Void)?

    /// The user chooses a preference only while no call owner exists. Once a
    /// Rust call projection identifies an owner, every command stays bound to
    /// that endpoint until Rust publishes an idle terminal state.
    private var callOwnerEndpoint: String?
    private var resolvedEndpoint: String { callOwnerEndpoint ?? endpoint }

    private struct DurableMutation: Codable, Equatable {
        let clientRequestID: String
        let path: String
        let action: String
        let selectedOwner: String
        let generation: UInt64
        let command: String?
        let number: String?
        /// The exact JSON bytes are retained because Rust fingerprints the raw
        /// payload. Re-encoding after a GUI restart could reorder JSON keys.
        let payload: Data
    }

    private static let pendingMutationKey = "switch.call.pendingMutation.v1"
    private let backend: BackendProcess
    private let defaults: CallCheckpointDefaults
    private var pendingMutation: DurableMutation?
    private var pendingCheckpointData: Data?
    private var corruptPendingCheckpoint = false
    private var checkpointPersistenceFailed = false
    private var pendingRequestInFlight = false
    private var readerGeneration: UInt64 = 0
    private var readerHistoryError: String?
    private var pollRevision: UInt64 = 0
    private var actionRevision: UInt64 = 0
    private var statusRefreshInFlight = false
    private var dtmfTask: Task<Void, Never>?
    private var pendingDTMF: [(digit: String, endpoint: String, generation: UInt64)] = []

    init(backend: BackendProcess, defaults: CallCheckpointDefaults = UserDefaults.standard) {
        self.backend = backend
        self.defaults = defaults
        if let stored = defaults.object(forKey: Self.pendingMutationKey) {
            guard let data = stored as? Data else {
                corruptPendingCheckpoint = true
                error = "通话操作恢复记录损坏；为避免重复呼叫，控制按钮保持锁定。"
                restorePendingFence()
                return
            }
            if let saved = try? JSONDecoder().decode(DurableMutation.self, from: data),
               Self.valid(saved) {
                pendingMutation = saved
                pendingCheckpointData = data
                endpoint = saved.selectedOwner
                if !confirmPendingPersistence() {
                    error = "无法确认通话请求编号已持久保存；为避免重复提交，控制按钮保持锁定。"
                }
            } else {
                corruptPendingCheckpoint = true
                error = "通话操作恢复记录损坏；为避免重复呼叫，控制按钮保持锁定。"
            }
        }
        restorePendingFence()
    }

    func reset() {
        pollRevision &+= 1
        actionRevision &+= 1
        statusRefreshInFlight = false
        answerInFlight = false
        dialInFlight = false
        rejectInFlight = false
        hangupInFlight = false
        readerGeneration = 0
        readerHistoryError = nil
        readerState = nil
        callOwnerEndpoint = nil
        error = corruptPendingCheckpoint
            ? "通话操作恢复记录损坏；为避免重复呼叫，控制按钮保持锁定。" : nil
        cancelDTMF()
        publish(CallStatus(state: "idle", number: nil, incoming: false, active: false))
        // reset/backend replacement cannot prove that a mutation did not cross
        // Rust's durable submit boundary. Restore the original fence and ID.
        restorePendingFence()
    }

    func acceptModuleStatus(_ newStatus: CallStatus) {
        guard callOwnerEndpoint == nil || callOwnerEndpoint == "module" else { return }
        if !newStatus.isIdle { bindCallOwner("module") }
        publish(newStatus)
    }

    func selectEndpoint(_ requested: String) {
        guard callOwnerEndpoint == nil, pendingMutation == nil else { return }
        endpoint = requested
    }

    func poll() {
        guard case .running = backend.state else { return }
        resumePendingMutation()
        guard !statusRefreshInFlight else { return }
        pollRevision &+= 1
        let revision = pollRevision
        statusRefreshInFlight = true
        Task {
            defer {
                if revision == pollRevision { statusRefreshInFlight = false }
            }
            var readerResponse: ReaderVoWiFiCallResponse?
            if readerAvailable?() == true || readerIMSRegistered?() == true
                || moduleVoWiFiActive?() == true || readerState != nil {
                readerResponse = try? await APIClient().get("api/vowifi/calls/status")
                guard revision == pollRevision else { return }
                if let readerResponse {
                    readerHistoryError = readerResponse.historyError
                    readerState = readerResponse.call
                    readerGeneration = readerResponse.generation
                    if Self.activeReaderStates.contains(readerResponse.call.state) {
                        bindCallOwner(readerResponse.source == "module" ? "module-vowifi" : "reader")
                    } else if readerResponse.call.state == "ended" {
                        clearCallOwnerIfMatchesVoWiFi()
                    }
                }
            } else {
                guard revision == pollRevision else { return }
                readerState = nil
                readerGeneration = 0
            }

            if Self.isVoWiFiEndpoint(resolvedEndpoint) {
                publishReaderStatus(readerResponse?.call ?? readerState)
                return
            }
            do {
                let response: CallStatus = try await APIClient().get("api/call/status")
                guard revision == pollRevision else { return }
                publish(response)
            } catch {
                guard revision == pollRevision else { return }
                publish(CallStatus(state: "unknown", number: status.number, incoming: false, active: false))
            }
        }
    }

    func dial(_ number: String) {
        let trimmed = number.trimmingCharacters(in: .whitespacesAndNewlines)
        guard case .running = backend.state, !trimmed.isEmpty,
              status.isIdle, !commandInFlight else { return }
        let selectedEndpoint = resolvedEndpoint
        let generation = readerGeneration
        _ = beginAction()
        error = nil
        let requestID = UUID().uuidString.lowercased()
        let request = CallDialRequest(
            clientRequestID: requestID, number: trimmed, preferredOwner: selectedEndpoint,
            observedOwner: callOwnerEndpoint, generation: generation)
        guard let mutation = checkpoint(
            requestID: requestID, path: "api/call/dial", action: "dial",
            owner: selectedEndpoint, generation: generation, command: nil,
            number: trimmed, request: request)
        else { return }
        dialInFlight = true
        launch(mutation)
    }

    func answer() {
        guard case .running = backend.state, !commandInFlight else { return }
        let selectedEndpoint = resolvedEndpoint
        guard status.isIncoming else {
            error = "当前没有可接听的来电。"
            return
        }
        if Self.isVoWiFiEndpoint(selectedEndpoint), !ReaderCallReadiness.canAnswer(readerState) {
            error = "VoWiFi 来电信令尚未就绪，暂时不能接听。"
            return
        }
        guard Self.isVoWiFiEndpoint(selectedEndpoint) || moduleCallModeReady?() == true else {
            error = "通话模式尚未就绪，暂时不能接听。"
            return
        }
        let generation = readerGeneration
        _ = beginAction()
        error = nil
        let requestID = UUID().uuidString.lowercased()
        let request = CallControlRequest(
            clientRequestID: requestID, preferredOwner: selectedEndpoint,
            observedOwner: callOwnerEndpoint, generation: generation, command: nil)
        guard let mutation = checkpoint(
            requestID: requestID, path: "api/call/answer", action: "answer",
            owner: selectedEndpoint, generation: generation, command: nil,
            number: status.number, request: request)
        else { return }
        answerInFlight = true
        launch(mutation)
    }

    func hangup() {
        guard case .running = backend.state, !commandInFlight else { return }
        let selectedEndpoint = resolvedEndpoint
        let generation = readerGeneration
        _ = beginAction()
        error = nil
        cancelDTMF()
        onPrepareHangup?(selectedEndpoint)
        let requestID = UUID().uuidString.lowercased()
        let request = CallControlRequest(
            clientRequestID: requestID, preferredOwner: selectedEndpoint,
            observedOwner: callOwnerEndpoint, generation: generation, command: nil)
        guard let mutation = checkpoint(
            requestID: requestID, path: "api/call/hangup", action: "hangup",
            owner: selectedEndpoint, generation: generation, command: nil,
            number: status.number, request: request)
        else { return }
        hangupInFlight = true
        launch(mutation)
    }

    /// Rejects a ringing call without treating the not-yet-established dialog as
    /// an active call. Reader VoWiFi must answer the pending INVITE with a final
    /// SIP response; sending BYE here is invalid and leaves the call ringing.
    func reject() {
        guard case .running = backend.state, !commandInFlight else { return }
        guard status.isIncoming else {
            error = "当前没有可拒接的来电。"
            return
        }
        let selectedEndpoint = resolvedEndpoint
        let generation = readerGeneration
        _ = beginAction()
        error = nil
        cancelDTMF()
        let requestID = UUID().uuidString.lowercased()
        let request = CallControlRequest(
            clientRequestID: requestID, preferredOwner: selectedEndpoint,
            observedOwner: callOwnerEndpoint, generation: generation, command: "reject")
        guard let mutation = checkpoint(
            requestID: requestID, path: "api/call/hangup", action: "hangup",
            owner: selectedEndpoint, generation: generation, command: "reject",
            number: status.number, request: request)
        else { return }
        rejectInFlight = true
        launch(mutation)
    }

    func sendDTMF(_ digit: String) {
        guard digit.count == 1, "0123456789*#".contains(digit) else { return }
        guard status.isActive, !commandInFlight else {
            error = "只有通话接通且没有待确认命令时才能发送按键。"
            return
        }
        let selectedEndpoint = resolvedEndpoint
        pendingDTMF.append((digit, selectedEndpoint, readerGeneration))
        guard dtmfTask == nil else { return }
        let revision = actionRevision
        dtmfTask = Task { [weak self] in
            guard let self else { return }
            defer { self.dtmfTask = nil }
            while !Task.isCancelled,
                  revision == self.actionRevision,
                  self.status.isActive,
                  !self.pendingDTMF.isEmpty {
                let next = self.pendingDTMF.removeFirst()
                let requestID = UUID().uuidString.lowercased()
                let request = CallDTMFRequest(
                    clientRequestID: requestID, digit: next.digit,
                    preferredOwner: next.endpoint, observedOwner: self.callOwnerEndpoint,
                    generation: next.generation)
                do {
                    let payload = try JSONEncoder().encode(request)
                    if Self.isVoWiFiEndpoint(next.endpoint) {
                        let response: ReaderVoWiFiCallResponse = try await APIClient(timeoutInterval: 5).upload(
                            "api/call/dtmf", data: payload)
                        guard revision == self.actionRevision else { return }
                        self.readerHistoryError = response.historyError
                        self.readerState = response.call
                        self.readerGeneration = response.generation
                    } else {
                        let _: CallActionResult = try await APIClient(timeoutInterval: 5).upload(
                            "api/call/dtmf", data: payload)
                        guard revision == self.actionRevision else { return }
                    }
                } catch {
                    guard !Task.isCancelled, revision == self.actionRevision else { return }
                    if mutationFailureRequiresStatusReconciliation(error) {
                        self.error = "按键 \(next.digit) 的提交结果未知；为避免重复，未自动重放。"
                        self.pendingDTMF.removeAll(keepingCapacity: true)
                        return
                    }
                    self.error = "发送按键 \(next.digit) 失败：\(error.localizedDescription)"
                }
            }
            if !self.status.isActive { self.pendingDTMF.removeAll(keepingCapacity: true) }
        }
    }

    func cancelDTMF() {
        dtmfTask?.cancel()
        dtmfTask = nil
        pendingDTMF.removeAll(keepingCapacity: true)
    }

    private func checkpoint<Request: Encodable>(
        requestID: String, path: String, action: String, owner: String,
        generation: UInt64, command: String?, number: String?, request: Request
    ) -> DurableMutation? {
        do {
            let payload = try JSONEncoder().encode(request)
            let mutation = DurableMutation(
                clientRequestID: requestID, path: path, action: action,
                selectedOwner: owner, generation: generation, command: command,
                number: number, payload: payload)
            let checkpoint = try JSONEncoder().encode(mutation)
            defaults.set(checkpoint, forKey: Self.pendingMutationKey)
            pendingMutation = mutation
            pendingCheckpointData = checkpoint
            endpoint = owner
            restorePendingFence()
            guard confirmPendingPersistence() else {
                error = "无法持久保存通话请求编号；为避免重复提交，控制按钮保持锁定。"
                return nil
            }
            return mutation
        } catch {
            self.error = "无法创建通话请求恢复记录：\(error.localizedDescription)"
            return nil
        }
    }

    private func launch(_ mutation: DurableMutation, recovery: Bool = false) {
        guard pendingMutation?.clientRequestID == mutation.clientRequestID,
              confirmPendingPersistence(),
              !pendingRequestInFlight else { return }
        pendingRequestInFlight = true
        let revision = actionRevision
        Task {
            defer { pendingRequestInFlight = false }
            await execute(mutation, revision: revision, recovery: recovery)
        }
    }

    private func resumePendingMutation() {
        guard let pendingMutation, !checkpointPersistenceFailed,
              confirmPendingPersistence(), !pendingRequestInFlight else { return }
        restorePendingFence()
        launch(pendingMutation, recovery: true)
    }

    private func execute(_ mutation: DurableMutation, revision: UInt64, recovery: Bool) async {
        do {
            if Self.isVoWiFiEndpoint(mutation.selectedOwner) {
                let response: ReaderVoWiFiCallResponse = try await APIClient().upload(
                    mutation.path, data: mutation.payload)
                guard pendingMutation?.clientRequestID == mutation.clientRequestID else { return }
                complete(mutation)
                guard revision == actionRevision else { return }
                if recovery {
                    // A replay receipt can be older than a fast terminal call
                    // projection. Clear only its exact fence; live polling owns
                    // the current call state and must not be rolled backwards.
                    poll()
                    return
                }
                if mutation.action == "dial" || mutation.action == "answer" {
                    bindCallOwner(response.source == "module" ? "module-vowifi" : "reader")
                }
                readerHistoryError = response.historyError
                readerState = response.call
                readerGeneration = response.generation
                if mutation.action == "dial" { publishReaderStatus(response.call) }
            } else {
                let response: CallActionResult = try await APIClient().upload(
                    mutation.path, data: mutation.payload)
                guard pendingMutation?.clientRequestID == mutation.clientRequestID else { return }
                guard response.accepted else {
                    complete(mutation)
                    guard revision == actionRevision else { return }
                    error = moduleNegativeAcknowledgement(for: mutation)
                    if mutation.action == "hangup" { poll() }
                    return
                }
                complete(mutation)
                guard revision == actionRevision else { return }
                if recovery {
                    poll()
                    return
                }
                if mutation.action == "dial" {
                    bindCallOwner("module")
                    publish(CallStatus(
                        state: "dialing", number: mutation.number,
                        incoming: false, active: false))
                }
                if mutation.action == "hangup",
                   let warning = response.warning, !warning.isEmpty {
                    onModuleHangupWarning?(warning)
                }
            }
            poll()
        } catch {
            guard pendingMutation?.clientRequestID == mutation.clientRequestID else { return }
            if mutationFailureRequiresStatusReconciliation(error) {
                self.error = "\(actionName(mutation))结果待确认；正在使用原请求编号恢复，确认前不会再次提交。"
                restorePendingFence()
                return
            }
            complete(mutation)
            guard revision == actionRevision else { return }
            self.error = "\(actionName(mutation))失败：\(error.localizedDescription)"
            if mutation.action == "hangup" { poll() }
        }
    }

    private func complete(_ mutation: DurableMutation) {
        guard pendingMutation?.clientRequestID == mutation.clientRequestID else { return }
        let retained = pendingCheckpointData
        defaults.removeObject(forKey: Self.pendingMutationKey)
        guard defaults.synchronize(), defaults.object(forKey: Self.pendingMutationKey) == nil else {
            if let retained {
                defaults.set(retained, forKey: Self.pendingMutationKey)
                _ = defaults.synchronize()
            }
            checkpointPersistenceFailed = true
            error = "通话回执已返回，但本地请求围栏未能安全清除；控制按钮保持锁定。"
            restorePendingFence()
            return
        }
        pendingMutation = nil
        pendingCheckpointData = nil
        dialInFlight = false
        answerInFlight = false
        rejectInFlight = false
        hangupInFlight = false
    }

    private func restorePendingFence() {
        guard let mutation = pendingMutation else {
            if corruptPendingCheckpoint { hangupInFlight = true }
            return
        }
        dialInFlight = mutation.action == "dial"
        answerInFlight = mutation.action == "answer"
        rejectInFlight = mutation.action == "hangup" && mutation.command == "reject"
        hangupInFlight = mutation.action == "hangup" && mutation.command != "reject"
    }

    private func confirmPendingPersistence() -> Bool {
        guard !checkpointPersistenceFailed,
              let expected = pendingCheckpointData,
              defaults.synchronize(),
              defaults.object(forKey: Self.pendingMutationKey) as? Data == expected else {
            checkpointPersistenceFailed = true
            return false
        }
        return true
    }

    private func moduleNegativeAcknowledgement(for mutation: DurableMutation) -> String {
        switch (mutation.action, mutation.command) {
        case ("dial", _): "模块没有确认接受拨号请求。"
        case ("answer", _): "模块没有确认接受接听请求。"
        case ("hangup", "reject"): "模块没有确认接受拒接请求。"
        default: "模块没有确认接受挂断请求。"
        }
    }

    private func actionName(_ mutation: DurableMutation) -> String {
        switch (mutation.action, mutation.command) {
        case ("dial", _): "拨号"
        case ("answer", _): "接听"
        case ("hangup", "reject"): "拒接"
        default: "挂断"
        }
    }

    private static func valid(_ mutation: DurableMutation) -> Bool {
        guard !mutation.clientRequestID.isEmpty,
              ["api/call/dial", "api/call/answer", "api/call/hangup"].contains(mutation.path),
              ["dial", "answer", "hangup"].contains(mutation.action),
              ["module", "reader", "module-vowifi"].contains(mutation.selectedOwner),
              let object = try? JSONSerialization.jsonObject(with: mutation.payload),
              let body = object as? [String: Any],
              body["client_request_id"] as? String == mutation.clientRequestID,
              body["preferred_owner"] as? String == mutation.selectedOwner,
              (body["generation"] as? NSNumber)?.uint64Value == mutation.generation
        else { return false }
        switch mutation.action {
        case "dial":
            return mutation.path == "api/call/dial"
                && body["number"] as? String == mutation.number
                && mutation.command == nil
        case "answer":
            return mutation.path == "api/call/answer" && mutation.command == nil
        case "hangup":
            return mutation.path == "api/call/hangup"
                && (mutation.command == nil || mutation.command == "reject")
                && body["command"] as? String == mutation.command
        default:
            return false
        }
    }

    private func beginAction() -> UInt64 {
        // An action changes the expected signalling state. Any poll that
        // started before it must not publish its now-stale response.
        actionRevision &+= 1
        pollRevision &+= 1
        statusRefreshInFlight = false
        dialInFlight = false
        answerInFlight = false
        rejectInFlight = false
        hangupInFlight = false
        return actionRevision
    }

    private func publishReaderStatus(_ state: ReaderVoWiFiCallState?) {
        let rawState = state?.state ?? "idle"
        let displayState: String
        switch rawState {
        case "outgoing", "connecting": displayState = "dialing"
        case "early": displayState = "alerting"
        case "ended": displayState = "idle"
        case "failed":
            displayState = "idle"
            if let detail = state?.lastError, !detail.isEmpty { error = "呼叫失败：\(detail)" }
        default: displayState = rawState
        }
        publish(CallStatus(
            state: displayState,
            number: state?.peer,
            incoming: rawState == "incoming",
            active: rawState == "active",
            historyError: readerHistoryError))
    }

    private func publish(_ newStatus: CallStatus) {
        let previous = status
        status = newStatus
        if newStatus.isIdle && !previous.isIdle {
            cancelDTMF()
            callOwnerEndpoint = nil
        }
        onStatusChange?(previous, newStatus)
    }

    private func bindCallOwner(_ owner: String) {
        if let current = callOwnerEndpoint, current != owner { return }
        callOwnerEndpoint = owner
        endpoint = owner
    }

    private func clearCallOwnerIfMatchesVoWiFi() {
        guard let owner = callOwnerEndpoint, Self.isVoWiFiEndpoint(owner) else { return }
        callOwnerEndpoint = nil
    }

    private static let activeReaderStates: Set<String> = [
        "incoming", "outgoing", "early", "connecting", "active",
    ]

    private static func isVoWiFiEndpoint(_ endpoint: String) -> Bool {
        endpoint == "reader" || endpoint == "module-vowifi"
    }
}
