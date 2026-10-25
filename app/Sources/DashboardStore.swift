import Foundation
import Combine

/// 首页数据缓存：后台持续轮询后端，页面切换时立即展示最近一次的数据，
/// 避免每次进入首页都重新拉取导致"读取模块状态…"闪烁。
@MainActor
final class DashboardStore: ObservableObject {
    /// 供 AppDelegate（通知点击）等非视图位置访问
    static weak var shared: DashboardStore?
    let deviceSnapshot: DeviceSnapshotStore
    var moduleState: DashboardModuleState { deviceSnapshot.state }
    let moduleNetwork: ModuleNetworkStore
    var unifiedStateRevision: UnifiedStateRevision {
        UnifiedStateRevision(
            device: deviceStateRevision,
            reader: readerRuntime.revision,
            voWiFi: voWiFiStateRevision)
    }

    let networkServices: NetworkServicesStore
    let callHistoryStore: CallHistoryStore
    let callMode: CallModeStore
    let callRuntime: CallRuntimeStore
    let callAudio: CallAudioStore
    let moduleVoWiFi: ModuleVoWiFiStore

    @Published var toast: ToastItem?

    let readerRuntime: ReaderRuntimeStore
    /// 本次来电时间（用于详情展示）
    @Published var incomingAt: Date?

    private let backend: BackendProcess
    private var timer: Timer?
    private var callTimer: Timer?
    private var cancellables = Set<AnyCancellable>()

    init(backend: BackendProcess) {
        self.backend = backend
        self.deviceSnapshot = DeviceSnapshotStore(backend: backend)
        self.moduleNetwork = ModuleNetworkStore(backend: backend)
        self.networkServices = NetworkServicesStore(backend: backend)
        self.callHistoryStore = CallHistoryStore(backend: backend)
        self.readerRuntime = ReaderRuntimeStore(backend: backend)
        self.callMode = CallModeStore(backend: backend)
        self.callRuntime = CallRuntimeStore(backend: backend)
        self.callAudio = CallAudioStore(backend: backend)
        self.moduleVoWiFi = ModuleVoWiFiStore(backend: backend)
        DashboardStore.shared = self
        networkServices.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        moduleNetwork.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        moduleNetwork.isCallModeBusy = { [weak self] in
            self?.callModeStatus?.isBusy == true
        }
        moduleNetwork.onModuleStatus = { [weak self] status in
            self?.deviceSnapshot.commitModuleStatus(status)
        }
        moduleNetwork.onCallStatus = { [weak self] status in
            self?.callRuntime.acceptModuleStatus(status)
        }
        moduleNetwork.onServicesReload = { [weak self] in
            self?.networkServices.load()
        }
        moduleNetwork.onDashboardRefresh = { [weak self] in
            self?.refresh()
        }
        moduleNetwork.onToast = { [weak self] message, isSuccess, title in
            self?.showToast(message: message, isSuccess: isSuccess, title: title)
        }
        deviceSnapshot.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        deviceSnapshot.readerGeneration = { [weak self] in self?.readerRuntime.generation ?? 0 }
        deviceSnapshot.readerActionInFlight = { [weak self] in self?.readerActionInFlight == true }
        deviceSnapshot.applyReaderSnapshot = { [weak self] snapshot, issues, generation, instanceID in
            self?.readerRuntime.applySnapshot(
                snapshot,
                issues: issues,
                expectedGeneration: generation,
                autoConnect: true,
                instanceID: instanceID)
        }
        deviceSnapshot.refreshReaderFallback = { [weak self] in
            self?.readerRuntime.refreshAfterAggregateFailure(autoConnect: true)
        }
        deviceSnapshot.onTraffic = { [weak self] traffic in
            self?.moduleNetwork.updateTraffic(traffic)
        }
        callHistoryStore.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        callHistoryStore.onError = { [weak self] message in
            self?.showToast(message: message, isSuccess: false, title: "通话记录")
        }
        readerRuntime.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        readerRuntime.onToast = { [weak self] message, isSuccess, title in
            self?.showToast(message: message, isSuccess: isSuccess, title: title)
        }
        callMode.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        callMode.isCallActive = { [weak self] in self?.callStatus.isActive == true }
        callMode.isAudioRunning = { [weak self] in self?.callAudio.running == true }
        callMode.onStartAudio = { [weak self] in self?.callAudio.startModuleAudio() }
        callMode.onToast = { [weak self] message, isSuccess, title in
            self?.showToast(message: message, isSuccess: isSuccess, title: title)
        }
        callRuntime.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        callRuntime.readerAvailable = { [weak self] in self?.readerConnected == true }
        callRuntime.readerIMSRegistered = { [weak self] in
            self?.readerVoWiFiStatus?.imsState == "registered"
        }
        callRuntime.moduleVoWiFiActive = { [weak self] in
            self?.voWiFiStatus?.vowifiActive == true
        }
        callRuntime.moduleCallModeReady = { [weak self] in self?.callModeStatus?.isReady == true }
        callRuntime.onStatusChange = { [weak self] previous, status in
            self?.updateCallStatus(previous: previous, status: status)
        }
        callRuntime.onPrepareHangup = { [weak self] endpoint in
            self?.callAudio.prepareForHangup(endpoint: endpoint)
        }
        callRuntime.onModuleHangupWarning = { [weak self] warning in
            self?.callAudio.error = warning
        }
        callAudio.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        callAudio.currentStatus = { [weak self] in
            self?.callStatus ?? CallStatus(state: "idle", number: nil, incoming: false, active: false)
        }
        callAudio.currentEndpoint = { [weak self] in self?.callEndpoint ?? "module" }
        callAudio.currentReaderState = { [weak self] in self?.readerCallState }
        callAudio.moduleCallModeReady = { [weak self] in self?.callModeStatus?.isReady == true }
        moduleVoWiFi.objectWillChange
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        moduleVoWiFi.moduleStatus = { [weak self] in self?.status }
        moduleVoWiFi.connectionIntent = { [weak self] in
            guard let self else { return .automatic }
            return ModuleVoWiFiConnectionIntent.sharedProxyUsingAutomaticCarrierTemplate(
                proxyHost: self.readerRuntime.proxyIP,
                proxyPort: self.readerRuntime.proxyPort)
        }
        moduleVoWiFi.onToast = { [weak self] message, isSuccess, title in
            self?.showToast(message: message, isSuccess: isSuccess, title: title)
        }
        moduleVoWiFi.$state
            .map { $0.status?.airplaneMode }
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.moduleNetwork.refreshFlightMode() }
            .store(in: &cancellables)
        deviceSnapshot.$state
            .map { $0.status?.iccid }
            .removeDuplicates()
            .receive(on: RunLoop.main)
            .sink { [weak self] iccid in
                self?.moduleVoWiFi.observeActiveProfileICCID(iccid)
            }
            .store(in: &cancellables)
        backend.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                guard let self else { return }
                switch state {
                case .running:
                    self.startPolling()
                    self.networkServices.load()
                    self.pollCallModeStatus()
                default:
                    self.stopPolling()
                    self.reset()
                }
            }
            .store(in: &cancellables)
    }

    private func reset() {
        moduleNetwork.reset()
        deviceSnapshot.reset()
        networkServices.reset()
        toast = nil
        callMode.reset()
        callRuntime.reset()
        callAudio.reset()
        readerRuntime.reset()
        incomingAt = nil
        IncomingCallCard.shared.hide()
    }

    private func startPolling() {
        guard timer == nil else { return }
        let generalTimer = Timer(timeInterval: 2, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self, !self.moduleNetwork.recovering else { return }
                self.refresh()
                self.pollCallModeStatus()
                if self.readerConnected,
                   !self.readerVoWiFiActionInFlight,
                   Date().timeIntervalSince(self.readerRuntime.lastVoWiFiHealthRefreshAt ?? .distantPast) >= 10 {
                    self.readerRuntime.markVoWiFiHealthRefresh()
                    self.refreshReaderVoWiFi()
                }
                if self.voWiFiStatus?.imsWiFiCallingEnabled == true,
                   !self.voWiFiActionInFlight,
                   Date().timeIntervalSince(self.moduleVoWiFi.lastHealthRefreshAt ?? .distantPast) >= 10 {
                    self.refreshVoWiFi()
                }
            }
        }
        timer = generalTimer
        RunLoop.main.add(generalTimer, forMode: .common)

        // Incoming calls are independent from module recovery and audio setup.
        // Poll on the common run-loop mode so menus, sheets and text selection
        // cannot suspend MT call detection.
        let incomingTimer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.pollCallStatus()
            }
        }
        callTimer = incomingTimer
        RunLoop.main.add(incomingTimer, forMode: .common)
        refresh()
        pollCallStatus()
        pollCallModeStatus()
    }

    private func stopPolling() {
        timer?.invalidate()
        timer = nil
        callTimer?.invalidate()
        callTimer = nil
    }

    // MARK: - 数据刷新

    func refresh() {
        deviceSnapshot.refresh()
    }

    // MARK: - USB 网卡

    func loadUSBStatus() {
        moduleNetwork.loadUSBStatus()
    }

    func switchMode() {
        moduleNetwork.switchMode()
    }

    func reboot() {
        moduleNetwork.reboot()
    }

    var canRetryNetworkConnection: Bool {
        moduleNetwork.canRetryConnection
    }

    func recoverNetworkAfterWake() {
        moduleNetwork.recoverAfterWake()
    }

    func retryNetworkConnection() {
        moduleNetwork.retryConnection()
    }

    func runCheck4G() {
        moduleNetwork.runCheck4G()
    }

    // MARK: - 语音与通话

    func pollCallModeStatus() {
        guard !callAudio.activationInFlight else { return }
        callMode.poll()
    }

    func enableCallMode(confirmADBAuthorization: Bool) {
        callMode.enable(confirmADBAuthorization: confirmADBAuthorization)
    }

    func downloadCallModeRuntime(source: String) {
        callMode.downloadRuntime(source: source)
    }

    func retryCallModePreparation() {
        callMode.retryPreparation()
    }

    func loadCallModeBackups() {
        callMode.loadBackups()
    }

    func exportCallModeBackup(_ backup: CallModeUSBBackupSummary, to destination: URL) {
        callMode.exportBackup(backup, to: destination)
    }

    func importCallModeBackup(from source: URL) {
        callMode.importBackup(from: source)
    }

    func deleteCallModeBackup(_ backup: CallModeUSBBackupSummary) {
        callMode.deleteBackup(backup)
    }

    func restoreCallModeBackup(_ backup: CallModeUSBBackupSummary) {
        callMode.restoreBackup(backup)
    }

    /// 轮询通话状态（并入 2 秒刷新）
    func pollCallStatus() {
        callRuntime.poll()
    }

    private func updateCallStatus(previous: CallStatus, status: CallStatus) {
        // 新来电（从非来电变为来电）：弹出自定义通知卡片并响铃
        if status.isIncoming && !previous.isIncoming {
            incomingAt = Date()
            IncomingCallCard.shared.show(store: self)
        }
        if previous.isIncoming && !status.isIncoming {
            IncomingCallCard.shared.hide()
        }
        callAudio.handleStatusChange(previous: previous, status: status)
        // 通话结束（非空闲 → 空闲）时清空来电信息
        if status.isIdle && !previous.isIdle {
            incomingAt = nil
            IncomingCallCard.shared.hide()
            loadCallHistory()
        }
    }

    // MARK: - 通话记录

    /// 拉取通话记录（后端按最新在前返回）
    func loadCallHistory() {
        callHistoryStore.load()
    }

    /// 清空全部通话记录
    func clearCallHistory() {
        callHistoryStore.clear()
    }

    /// 删除单条通话记录
    func deleteCallRecord(_ id: String) {
        callHistoryStore.delete(id)
    }

    func dial(_ number: String) {
        callRuntime.dial(number)
    }

    func answerCall() {
        callRuntime.answer()
    }

    func hangup() {
        callRuntime.hangup()
    }

    func rejectCall() {
        callRuntime.reject()
    }

    func sendDTMF(_ digit: String) {
        callRuntime.sendDTMF(digit)
    }

    // MARK: - VoWiFi

    func refreshVoWiFi() {
        moduleVoWiFi.refresh()
    }

    func enableVoWiFi() {
        moduleVoWiFi.enable()
    }

    func disableVoWiFi() {
        moduleVoWiFi.disable()
    }

    // MARK: - 读卡器（PC/SC CCID）

    func refreshReaderList(autoConnect: Bool = true, refreshConnectedInfo: Bool = true) {
        readerRuntime.refreshList(
            autoConnect: autoConnect,
            refreshConnectedInfo: refreshConnectedInfo)
    }

    func connectReader(_ name: String) {
        readerRuntime.connect(name)
    }

    func disconnectReader() {
        readerRuntime.disconnect()
    }

    func refreshSIM() {
        readerRuntime.refreshSIM()
    }

    func clearReaderRuntimeState(clearList: Bool = false) {
        readerRuntime.clearRuntimeState(clearList: clearList)
    }

    func updateReaderProfileOperatorName(_ name: String?) {
        readerRuntime.updateProfileOperatorName(name)
    }

    var readerOperatorDisplayName: String {
        readerRuntime.operatorDisplayName
    }

    func synchronizeReaderAfterProfileSwitch(_ snapshot: ReaderInfoResponse? = nil) {
        readerRuntime.synchronizeAfterProfileSwitch(snapshot)
    }

    func refreshReaderVoWiFi(force: Bool = false) {
        readerRuntime.refreshVoWiFi(force: force)
    }

    func enableReaderVoWiFi() {
        readerRuntime.enableVoWiFi()
    }

    func disableReaderVoWiFi() {
        readerRuntime.disableVoWiFi()
    }

    func saveProxyAndCheck() {
        readerRuntime.checkProxy()
    }

    func saveEPDGConfig() {
        readerRuntime.saveEPDGConfig()
    }

    // MARK: - 提示气泡

    private var toastDismissTask: Task<Void, Never>?

    private func showToast(message: String, isSuccess: Bool, title: String? = nil) {
        toast = ToastItem(message: message, isSuccess: isSuccess, title: title)
        scheduleToastDismiss()
    }

    func notifyCopied() {
        showToast(message: "该字段已写入剪贴板", isSuccess: true, title: "已复制")
    }

    /// 鼠标悬停时延长显示时间
    func extendToast() {
        scheduleToastDismiss()
    }

    private func scheduleToastDismiss() {
        toastDismissTask?.cancel()
        toastDismissTask = Task {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if !Task.isCancelled {
                toast = nil
            }
        }
    }

}
