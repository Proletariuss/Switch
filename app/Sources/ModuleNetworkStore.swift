import Combine
import Foundation

enum NetworkRecoveryTrigger: String, Codable, Equatable {
    case wake
    case manual
    case reboot
}

struct NetworkRecoveryRequest: Encodable {
    let trigger: NetworkRecoveryTrigger
    let clientRequestID: String

    enum CodingKeys: String, CodingKey {
        case trigger
        case clientRequestID = "client_request_id"
    }
}

struct NetworkRecoveryPendingIntent: Codable, Equatable {
    let clientRequestID: String
    let trigger: NetworkRecoveryTrigger
}

protocol NetworkRecoveryCheckpointing: AnyObject {
    func load() throws -> NetworkRecoveryPendingIntent?
    func save(_ pending: NetworkRecoveryPendingIntent) throws
    func clear(matching clientRequestID: String) throws
}

enum NetworkRecoveryCheckpointError: LocalizedError {
    case invalid(String)
    case persistence(String)

    var errorDescription: String? {
        switch self {
        case .invalid(let detail): return "网络恢复请求记录损坏：\(detail)"
        case .persistence(let detail): return "网络恢复请求记录持久化未确认：\(detail)"
        }
    }
}

final class UserDefaultsNetworkRecoveryCheckpoint: NetworkRecoveryCheckpointing {
    private static let key = "switch.network-recovery.pending.v1"
    private let defaults: UserDefaults
    // A failed write/clear is uncertain even if UserDefaults' memory cache
    // appears updated. Do not let a later button press bypass that failure.
    private var persistenceFailure: NetworkRecoveryCheckpointError?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func load() throws -> NetworkRecoveryPendingIntent? {
        if let persistenceFailure { throw persistenceFailure }
        guard let stored = defaults.object(forKey: Self.key) else { return nil }
        guard let data = stored as? Data else {
            throw NetworkRecoveryCheckpointError.invalid("checkpoint is not Data")
        }
        let pending = try JSONDecoder().decode(NetworkRecoveryPendingIntent.self, from: data)
        guard !pending.clientRequestID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw NetworkRecoveryCheckpointError.invalid("request ID is empty")
        }
        return pending
    }

    func save(_ pending: NetworkRecoveryPendingIntent) throws {
        if let existing = try load(), existing != pending {
            throw NetworkRecoveryCheckpointError.invalid("refusing to replace a pending request")
        }
        guard !pending.clientRequestID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw NetworkRecoveryCheckpointError.invalid("request ID is empty")
        }
        let encoded = try JSONEncoder().encode(pending)
        defaults.set(encoded, forKey: Self.key)
        guard defaults.synchronize(), defaults.object(forKey: Self.key) as? Data == encoded,
              try load() == pending else {
            let failure = NetworkRecoveryCheckpointError.persistence("save synchronize/readback failed")
            persistenceFailure = failure
            throw failure
        }
    }

    func clear(matching clientRequestID: String) throws {
        guard let pending = try load() else { return }
        guard pending.clientRequestID == clientRequestID,
              let retained = defaults.object(forKey: Self.key) as? Data else {
            throw NetworkRecoveryCheckpointError.invalid("clear request ID mismatch")
        }
        defaults.removeObject(forKey: Self.key)
        guard defaults.synchronize(), defaults.object(forKey: Self.key) == nil else {
            // Restore the original pointer before reporting failure, including
            // to the next GUI instance. No automatic destructive repair occurs.
            defaults.set(retained, forKey: Self.key)
            _ = defaults.synchronize()
            let failure = NetworkRecoveryCheckpointError.persistence("clear synchronize/readback failed")
            persistenceFailure = failure
            throw failure
        }
    }
}

/// DJI 模块网络控制的单一状态源。
///
/// 流量、USB 网卡模式、诊断和重连均在此处串行化。DashboardStore 只负责
/// 把模块与通话状态回写到各自领域，避免多个页面各自启动重复网络任务。
@MainActor
final class ModuleNetworkStore: ObservableObject {
    @Published private(set) var traffic: TrafficSnapshot?
    // Last observed saved USB composition, not proof that macOS has applied it.
    @Published private(set) var usbnetEnabled = false
    @Published private(set) var usbnetLoaded = false
    @Published private(set) var busy = false
    @Published private(set) var recovering = false
    @Published private(set) var recoveryMessage: String?
    @Published private(set) var check4G: CheckResult?
    @Published private(set) var actionMessage: String?
    @Published private(set) var actionError: String?
    @Published private(set) var needsReboot = false
    @Published private(set) var flightModeStatus: ModuleFlightModeStatus?
    @Published private(set) var flightModeLoaded = false
    @Published private(set) var flightModeBusy = false
    @Published private(set) var flightModeError: String?

    var isCallModeBusy: () -> Bool = { false }
    var onModuleStatus: (DeviceStatus) -> Void = { _ in }
    var onCallStatus: (CallStatus) -> Void = { _ in }
    var onServicesReload: () -> Void = {}
    var onDashboardRefresh: () -> Void = {}
    var onToast: (_ message: String, _ isSuccess: Bool, _ title: String?) -> Void = { _, _, _ in }

    private let backend: BackendProcess
    private let recoveryCheckpoint: NetworkRecoveryCheckpointing
    private var loadTask: Task<Void, Never>?
    private var actionTask: Task<Void, Never>?
    private var recoveryTask: Task<Void, Never>?
    private var flightModeTask: Task<Void, Never>?
    private var lifecycleGeneration: UInt64 = 0
    private var loadRevision: UInt64 = 0
    private var flightModeRevision: UInt64 = 0
    private var recoveryRevision: UInt64 = 0
    private var cancellables = Set<AnyCancellable>()

    init(
        backend: BackendProcess,
        recoveryCheckpoint: NetworkRecoveryCheckpointing = UserDefaultsNetworkRecoveryCheckpoint()
    ) {
        self.backend = backend
        self.recoveryCheckpoint = recoveryCheckpoint
        backend.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                guard let self else { return }
                if case .running = state {
                    self.loadUSBStatus()
                    self.refreshFlightMode()
                    self.resumePendingRecovery()
                } else {
                    self.reset()
                }
            }
            .store(in: &cancellables)
    }

    deinit {
        loadTask?.cancel()
        actionTask?.cancel()
        recoveryTask?.cancel()
        flightModeTask?.cancel()
    }

    func updateTraffic(_ snapshot: TrafficSnapshot) {
        traffic = snapshot
    }

    func loadUSBStatus() {
        guard case .running = backend.state, !busy else { return }
        loadRevision &+= 1
        let expectedRevision = loadRevision
        let expectedGeneration = lifecycleGeneration
        loadTask?.cancel()
        loadTask = Task { [weak self] in
            do {
                let diagnostic: NetworkDiagnostic = try await APIClient().get("api/network")
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration,
                      self.loadRevision == expectedRevision else { return }
                if let mode = Int(diagnostic.usbnetMode ?? "") {
                    self.usbnetEnabled = mode == 1
                }
                self.usbnetLoaded = true
                self.loadTask = nil
            } catch {
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration,
                      self.loadRevision == expectedRevision else { return }
                // 已完成读取尝试；保留最近一次成功的开关值。
                self.usbnetLoaded = true
                self.loadTask = nil
            }
        }
    }

    func refreshFlightMode() {
        guard case .running = backend.state, !flightModeBusy else { return }
        flightModeRevision &+= 1
        let expectedRevision = flightModeRevision
        let expectedGeneration = lifecycleGeneration
        flightModeTask?.cancel()
        flightModeTask = Task { [weak self] in
            do {
                let status: ModuleFlightModeStatus = try await APIClient(timeoutInterval: 12)
                    .get("api/module/flight-mode")
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration,
                      self.flightModeRevision == expectedRevision else { return }
                self.flightModeStatus = status
                self.flightModeLoaded = true
                self.flightModeError = nil
                self.flightModeTask = nil
            } catch {
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration,
                      self.flightModeRevision == expectedRevision else { return }
                self.flightModeLoaded = true
                self.flightModeError = "读取模块飞行模式失败：\(error.localizedDescription)"
                self.flightModeTask = nil
            }
        }
    }

    func setFlightMode(enabled: Bool) {
        guard case .running = backend.state, !flightModeBusy else { return }
        guard flightModeStatus?.available == true else {
            onToast("模块未连接，无法切换飞行模式", false, "模块飞行模式")
            refreshFlightMode()
            return
        }
        guard flightModeStatus?.canToggle == true else {
            onToast(flightModeStatus?.detail ?? "模块当前正被其他业务占用", false, "暂时无法切换")
            refreshFlightMode()
            return
        }
        flightModeRevision &+= 1
        let expectedRevision = flightModeRevision
        let expectedGeneration = lifecycleGeneration
        flightModeTask?.cancel()
        flightModeBusy = true
        flightModeError = nil
        flightModeTask = Task { [weak self] in
            guard let self else { return }
            do {
                let status: ModuleFlightModeStatus = try await APIClient(timeoutInterval: 30)
                    .send("api/module/flight-mode", body: ModuleFlightModeRequest(enabled: enabled, userInitiated: true))
                guard !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration,
                      self.flightModeRevision == expectedRevision else { return }
                self.flightModeBusy = false
                self.flightModeTask = nil
                self.flightModeStatus = status
                self.flightModeLoaded = true
                self.flightModeError = nil
                let detail = status.detail?.trimmingCharacters(in: .whitespacesAndNewlines)
                self.onToast(
                    detail?.isEmpty == false ? detail! : status.summary,
                    status.enabled == enabled,
                    "模块飞行模式")
            } catch {
                guard !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration,
                      self.flightModeRevision == expectedRevision else { return }
                self.flightModeBusy = false
                self.flightModeTask = nil
                self.flightModeError = error.localizedDescription
                self.onToast("\(enabled ? "开启" : "关闭")飞行模式失败：\(error.localizedDescription)", false, "模块飞行模式")
                self.refreshFlightMode()
            }
        }
    }

    func switchMode() {
        guard case .running = backend.state, !busy, !recovering else { return }
        let targetMode = usbnetEnabled ? 0 : 1
        // An older diagnostic must not overwrite the acknowledged target while
        // this operation waits for the radio owner. Never toggle optimistically.
        loadRevision &+= 1
        loadTask?.cancel()
        loadTask = nil
        beginAction()
        let expectedGeneration = lifecycleGeneration
        actionTask = Task { [weak self] in
            guard let self else { return }
            do {
                let result: USBNetResult = try await APIClient().send(
                    "api/network/usbnet",
                    body: USBNetRequest(mode: targetMode))
                guard !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.needsReboot = result.needsReboot == true
                self.actionMessage = self.needsReboot
                    ? "网卡模式设置已受理，需重启模块后生效；当前上网状态以实际连接为准。"
                    : "网卡模式设置已受理，正在核对配置与实际连接。"
                self.finishAction()
                self.loadUSBStatus()
                self.onServicesReload()
                self.onToast(self.actionMessage!, true, "模块网卡")
            } catch {
                guard !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.finishAction()
                self.actionError = "切换网卡配置失败：\(error.localizedDescription)"
                self.loadUSBStatus()
                self.onToast(self.actionError!, false, "模块网卡")
            }
        }
    }

    func reboot() {
        guard case .running = backend.state, !busy, !recovering else { return }
        guard flightModeStatus?.enabled != true else {
            onToast("请先关闭飞行模式，再重启模块", false, "无法重启")
            return
        }
        scheduleRecovery(.reboot)
    }

    var canRetryConnection: Bool {
        traffic?.recoveryRetryable == true
    }

    func recoverAfterWake() {
        scheduleRecovery(.wake)
    }

    func retryConnection() {
        scheduleRecovery(.manual)
    }

    func runCheck4G() {
        guard case .running = backend.state, !busy, !recovering else { return }
        beginAction()
        let expectedGeneration = lifecycleGeneration
        actionTask = Task { [weak self] in
            guard let self else { return }
            do {
                let result: CheckResult = try await APIClient().send("api/network/check-4g")
                guard !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.check4G = result
                self.finishAction()
                self.onToast(
                    [result.summary, result.detail].compactMap { $0 }.joined(separator: "\n"),
                    result.ok,
                    nil)
            } catch {
                guard !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.finishAction()
                self.onToast("检查失败：\(error.localizedDescription)", false, nil)
            }
        }
    }

    func reset() {
        lifecycleGeneration &+= 1
        recoveryRevision &+= 1
        loadRevision &+= 1
        loadTask?.cancel()
        actionTask?.cancel()
        recoveryTask?.cancel()
        flightModeTask?.cancel()
        loadTask = nil
        actionTask = nil
        recoveryTask = nil
        flightModeTask = nil
        traffic = nil
        usbnetEnabled = false
        usbnetLoaded = false
        busy = false
        recovering = false
        recoveryMessage = nil
        check4G = nil
        actionMessage = nil
        actionError = nil
        needsReboot = false
        flightModeStatus = nil
        flightModeLoaded = false
        flightModeBusy = false
        flightModeError = nil
    }

    private func beginAction() {
        actionTask?.cancel()
        actionTask = nil
        busy = true
        actionMessage = nil
        actionError = nil
    }

    private func finishAction() {
        busy = false
        actionTask = nil
    }

    private func resumePendingRecovery() {
        guard recoveryTask == nil else { return }
        do {
            guard let pending = try recoveryCheckpoint.load() else { return }
            scheduleRecovery(pending.trigger)
        } catch {
            // A damaged local request pointer must fail closed. Rust still owns
            // the durable receipt, but a new anonymous action is not allowed.
            recovering = true
            recoveryMessage = "网络恢复请求标识无法读取，请保留现场并检查诊断"
            actionError = error.localizedDescription
        }
    }

    private func scheduleRecovery(_ trigger: NetworkRecoveryTrigger) {
        guard case .running = backend.state else {
            if trigger != .wake {
                onToast("后台服务尚未运行", false, "无法重新连接")
            }
            return
        }
        guard recoveryTask == nil else { return }
        let pending: NetworkRecoveryPendingIntent
        do {
            if let retained = try recoveryCheckpoint.load() {
                // A restarted GUI always resumes the exact request already
                // admitted or possibly admitted by Rust. It never replaces the
                // request with the latest button/wake event.
                pending = retained
            } else {
                pending = NetworkRecoveryPendingIntent(
                    clientRequestID: UUID().uuidString.lowercased(),
                    trigger: trigger)
                try recoveryCheckpoint.save(pending)
            }
        } catch {
            recovering = true
            recoveryMessage = "网络恢复请求标识无法持久化，未提交模块重启"
            actionError = error.localizedDescription
            onToast(recoveryMessage!, false, "4G 网络")
            return
        }
        recoveryRevision &+= 1
        let expectedRevision = recoveryRevision
        recovering = true
        recoveryMessage = "正在请求网络恢复…"
        actionError = nil
        let expectedGeneration = lifecycleGeneration
        recoveryTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if self.lifecycleGeneration == expectedGeneration,
                   self.recoveryRevision == expectedRevision {
                    self.recoveryTask = nil
                }
            }
            let current = {
                !Task.isCancelled && self.lifecycleGeneration == expectedGeneration
                    && self.recoveryRevision == expectedRevision
            }
            let client = APIClient(timeoutInterval: 10)
            let path = pending.trigger == .reboot
                ? "api/network/reboot-module"
                : "api/network/recovery"
            let request = NetworkRecoveryRequest(
                trigger: pending.trigger,
                clientRequestID: pending.clientRequestID)
            while current() {
                do {
                    var operation: NetworkRecoveryOperation = try await client.send(path, body: request)
                    let operationID = operation.operationID
                    while current() {
                        guard operation.operationID == operationID else {
                            throw CocoaError(.coderReadCorrupt)
                        }
                        self.recoveryMessage = operation.phaseMessage
                        if operation.outcome != nil {
                            do {
                                try self.recoveryCheckpoint.clear(matching: pending.clientRequestID)
                            } catch {
                                self.recovering = true
                                self.recoveryMessage = "恢复回执已返回，但本地请求围栏未能安全清除；请保留现场并检查诊断"
                                self.actionError = error.localizedDescription
                                return
                            }
                            self.recovering = false
                            self.recoveryMessage = nil
                            self.actionError = nil
                            self.needsReboot = false
                            if pending.trigger != .wake || operation.outcome == "recovered" {
                                self.onToast(
                                    operation.resultMessage,
                                    operation.succeeded,
                                    pending.trigger == .reboot ? "模块重启" : "4G 网络")
                            }
                            self.loadUSBStatus()
                            self.onServicesReload()
                            self.onDashboardRefresh()
                            return
                        }
                        guard (100...5_000).contains(operation.pollAfterMS) else {
                            throw CocoaError(.coderReadCorrupt)
                        }
                        try await Task.sleep(for: .milliseconds(operation.pollAfterMS))
                        guard current() else { return }
                        operation = try await client.get(
                            "api/network/recovery?operation_id=\(operationID)")
                    }
                } catch {
                    guard current() else { return }
                    // A transport/waiter failure is not a terminal operation.
                    // Keep the UI fence and retry the exact durable ID.
                    self.recoveryMessage = "恢复回执暂不可读取，正在按原请求继续核对…"
                    self.actionError = error.localizedDescription
                    try? await Task.sleep(for: .seconds(1))
                }
            }
        }
    }
}
