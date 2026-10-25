import Foundation
import Combine
import AppKit
import UserNotifications

/// 短信数据缓存：后台持续轮询后端，检测到新收到的短信时弹出 macOS 系统通知。
@MainActor
final class SMSStore: ObservableObject {
    static let shared = SMSStore(
        backend: BackendProcess.shared,
        attentionStore: .shared)

    @Published var items: [SMSItem] = []
    @Published var status: SMSStatus?
    @Published var storage: SMSStorageResponse?
    @Published var devices: [SMSDeviceStatus] = []
    @Published var selectedSendDevice: String = UserDefaults.standard.string(forKey: "smsSelectedSendDevice") ?? "" {
        didSet { UserDefaults.standard.set(selectedSendDevice, forKey: "smsSelectedSendDevice") }
    }
    @Published var lastError: String?
    @Published var busy = false
    /// 当前是否正在查看短信页（正在查看时不再重复弹通知）
    @Published var viewingSMS = false
    /// 点击通知后要打开的会话号码（由短信页消费后清空）
    @Published var pendingOpenSender: String?
    @Published private(set) var pendingOpenLineID: String?
    @Published private(set) var pendingOpenMessageID: String?
    @Published private(set) var pendingOpenSubscriptionID: String?
    /// 接管短信保存模式（优先持久化到本机，不自动整库清空 SIM/模块存储）
    @Published private(set) var adoptionEnabled =
        UserDefaults.standard.bool(forKey: "smsAdopt")
    @Published private(set) var adoptionUpdating = false
    @Published private(set) var adoptionError: String?

    /// 新短信通知开关（默认开启，持久化到 UserDefaults）
    @Published var notificationsEnabled: Bool {
        didSet {
            UserDefaults.standard.set(notificationsEnabled, forKey: Self.enabledKey)
        }
    }

    private static let enabledKey = "smsNotificationsEnabled"
    private static let seenKey = "smsSeenIDs"
    private static let seenSeededKey = "smsSeenIDsSeeded"
    private static let seenOpaqueKey = "smsSeenIDsOpaqueV1"
    private static let forwardingIDsKey = "smsForwardingAcceptedIDsV1"
    private static let forwardingEnabledKey = "smsForwardingWasEnabledV1"

    private let backend: BackendProcess
    private let attentionStore: AttentionStore
    private var timer: Timer?
    private var refreshInFlight = false
    private var refreshTask: Task<Void, Never>?
    private var refreshGeneration: UInt64 = 0
    private var lifecycleGeneration: UInt64 = 0
    private var pendingForcedRefresh = false
    private var hasLoadedList = false
    private var cancellables = Set<AnyCancellable>()
    /// 首次拉取仅记录已存在的短信，不弹通知，避免启动时把历史短信全部通知一遍
    private var didSeed = false
    private var seenIDs = Set<String>()
    private var seenOrder: [String] = []
    /// 防止并发重复弹出授权框
    private var authRequestInFlight = false
    private var adoptionTask: Task<Void, Never>?
    private var adoptionGeneration: UInt64 = 0

    /// Snapshot the device-SMS producer's forwarding admission state without
    /// changing its live cursor or starting/stopping polling.
    func forwardingAdmissionSnapshot() -> DeviceSMSForwardingHandoff {
        DeviceSMSForwardingHandoff(
            acceptedNotificationIDs: UserDefaults.standard.stringArray(forKey: Self.forwardingIDsKey) ?? [],
            wasEnabled: UserDefaults.standard.object(forKey: Self.forwardingEnabledKey) as? Bool)
    }

    init(backend: BackendProcess, attentionStore: AttentionStore) {
        self.backend = backend
        self.attentionStore = attentionStore
        if UserDefaults.standard.object(forKey: Self.enabledKey) == nil {
            UserDefaults.standard.set(true, forKey: Self.enabledKey)
        }
        notificationsEnabled = UserDefaults.standard.bool(forKey: Self.enabledKey)
        seenOrder = UserDefaults.standard.stringArray(forKey: Self.seenKey) ?? []
        if !UserDefaults.standard.bool(forKey: Self.seenOpaqueKey) {
            seenOrder = seenOrder.map(SMSConversationIdentity.digest)
            UserDefaults.standard.set(seenOrder, forKey: Self.seenKey)
            UserDefaults.standard.set(true, forKey: Self.seenOpaqueKey)
        }
        seenIDs = Set(seenOrder)
        // Freeze the previous Swift producer's last durable cursor exactly
        // once. Current inbox notifications must not move this migration
        // snapshot before Rust has imported it.
        if UserDefaults.standard.object(forKey: Self.forwardingIDsKey) == nil {
            UserDefaults.standard.set(seenOrder, forKey: Self.forwardingIDsKey)
        }
        didSeed = UserDefaults.standard.bool(forKey: Self.seenSeededKey)
            || !seenOrder.isEmpty

        backend.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                guard let self else { return }
                switch state {
                case .running:
                    self.startPolling()
                    self.syncAdoption()
                default:
                    self.stopPolling()
                    self.cancelAdoptionRequest()
                }
            }
            .store(in: &cancellables)
    }

    deinit {
        refreshTask?.cancel()
        adoptionTask?.cancel()
    }

    // MARK: - 轮询

    private func startPolling() {
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.refresh()
            }
        }
        refresh()
    }

    private func stopPolling() {
        timer?.invalidate()
        timer = nil
        lifecycleGeneration &+= 1
        invalidateRefresh()
        hasLoadedList = false
    }

    private func invalidateRefresh() {
        refreshGeneration &+= 1
        refreshTask?.cancel()
        refreshTask = nil
        refreshInFlight = false
        pendingForcedRefresh = false
        busy = false
    }

    private func isCurrentRefresh(_ generation: UInt64) -> Bool {
        guard case .running = backend.state else { return false }
        return !Task.isCancelled && generation == refreshGeneration
    }

    // MARK: - 短信接管

    func syncAdoption() {
        guard case .running = backend.state else { return }
        adoptionTask?.cancel()
        adoptionGeneration &+= 1
        let generation = adoptionGeneration
        adoptionUpdating = true
        adoptionError = nil
        adoptionTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == adoptionGeneration {
                    adoptionUpdating = false
                    adoptionTask = nil
                }
            }
            struct AdoptResponse: Decodable { let enabled: Bool }
            for attempt in 0..<2 {
                do {
                    let response: AdoptResponse = try await APIClient().get("api/sms/adopt")
                    guard !Task.isCancelled, generation == adoptionGeneration else { return }
                    UserDefaults.standard.set(response.enabled, forKey: "smsAdopt")
                    adoptionEnabled = response.enabled
                    adoptionError = nil
                    return
                } catch {
                    guard !Task.isCancelled, generation == adoptionGeneration else { return }
                    if attempt == 0 {
                        try? await Task.sleep(nanoseconds: 500_000_000)
                    } else {
                        adoptionError = "短信接管状态同步失败：\(userFacingErrorMessage(error))"
                    }
                }
            }
        }
    }

    func setAdoptionEnabled(_ enabled: Bool) {
        guard case .running = backend.state,
              !adoptionUpdating,
              adoptionEnabled != enabled else { return }
        adoptionTask?.cancel()
        adoptionGeneration &+= 1
        let generation = adoptionGeneration
        let previous = adoptionEnabled
        UserDefaults.standard.set(enabled, forKey: "smsAdopt")
        adoptionEnabled = enabled
        adoptionUpdating = true
        adoptionError = nil
        adoptionTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == adoptionGeneration {
                    adoptionUpdating = false
                    adoptionTask = nil
                }
            }
            do {
                try await APIClient().send(
                    "api/sms/adopt",
                    body: SMSAdoptRequest(enabled: enabled))
            } catch {
                guard !Task.isCancelled, generation == adoptionGeneration else { return }
                UserDefaults.standard.set(previous, forKey: "smsAdopt")
                adoptionEnabled = previous
                adoptionError = "短信接管设置失败：\(userFacingErrorMessage(error))"
            }
        }
    }

    private func cancelAdoptionRequest() {
        adoptionTask?.cancel()
        adoptionTask = nil
        adoptionGeneration &+= 1
        adoptionUpdating = false
    }

    var readySendDevices: [SMSDeviceStatus] {
        SMSDeviceCatalog(devices: devices).readyDevices
    }

    func reconcileSendDeviceSelection() {
        let reconciled = SMSDeviceCatalog(devices: devices)
            .reconciledSelection(selectedSendDevice)
        if selectedSendDevice != reconciled {
            selectedSendDevice = reconciled
        }
    }

    func refresh(force: Bool = false) {
        guard case .running = backend.state else { return }
        guard !refreshInFlight else {
            // Coalesce user requests, but do not lose an explicit device read
            // just because the regular list poll is still in progress.
            pendingForcedRefresh = pendingForcedRefresh || force
            if force { busy = true }
            return
        }
        refreshGeneration &+= 1
        let generation = refreshGeneration
        refreshInFlight = true
        if force { busy = true }
        let client = APIClient()
        refreshTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if generation == refreshGeneration {
                    refreshInFlight = false
                    refreshTask = nil
                    busy = false
                    let forceAgain = pendingForcedRefresh
                    pendingForcedRefresh = false
                    if forceAgain { refresh(force: true) }
                }
            }
            do {
                async let statusRequest: SMSStatus = client.get("api/sms/status")
                async let devicesRequest: SMSDevicesResponse = client.get("api/sms/devices")
                let (st, deviceResponse) = try await (statusRequest, devicesRequest)
                guard isCurrentRefresh(generation) else { return }
                var refreshError: String?
                // An empty, successfully loaded mailbox is not an unloaded
                // mailbox. The backend already polls hardware; re-read only
                // initially, explicitly, or when a device becomes available.
                let deviceBecameReady = deviceResponse.devices.contains { device in
                    guard device.available else { return false }
                    guard let previous = self.devices.first(where: { $0.id == device.id }) else { return true }
                    return !previous.available || (!previous.ready && device.ready)
                        || previous.phone != device.phone
                }
                if force || !hasLoadedList || deviceBecameReady {
                    do {
                        let _: SMSRefreshResult = try await client.send("api/sms/refresh")
                    } catch {
                        refreshError = userFacingErrorMessage(error)
                    }
                }
                guard isCurrentRefresh(generation) else { return }
                async let listRequest: [SMSItem] = client.get("api/sms")
                async let storageRequest: SMSStorageResponse = client.get("api/sms/storage")
                let (list, storageRes) = try await (listRequest, storageRequest)
                guard isCurrentRefresh(generation) else { return }
                hasLoadedList = true
                status = st
                storage = storageRes
                devices = deviceResponse.devices
                reconcileSendDeviceSelection()
                let pollError = st.lastPollError
                if let refreshError, !isModuleDisconnectedError(refreshError) {
                    lastError = refreshError
                } else if let pollError, !isModuleDisconnectedError(pollError) {
                    lastError = pollError
                } else {
                    lastError = nil
                }
                items = list
                await handleNewMessages(list, generation: generation)
            } catch {
                guard isCurrentRefresh(generation) else { return }
                devices = []
                reconcileSendDeviceSelection()
                let msg = userFacingErrorMessage(error)
                lastError = isModuleDisconnectedError(msg) ? nil : msg
            }
        }
    }

    // MARK: - 新短信检测与通知

    /// 确保已获得通知授权（未决定时弹出系统授权框），返回是否可用
    func ensureAuthorization() async -> Bool {
        let center = UNUserNotificationCenter.current()
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral:
            return true
        case .notDetermined:
            guard !authRequestInFlight else { return false }
            authRequestInFlight = true
            defer { authRequestInFlight = false }
            do {
                return try await center.requestAuthorization(options: [.alert, .sound])
            } catch {
                return false
            }
        case .denied:
            return false
        @unknown default:
            return false
        }
    }

    private func handleNewMessages(_ list: [SMSItem], generation: UInt64) async {
        guard isCurrentRefresh(generation) else { return }
        let incoming = list.filter { !$0.isOutgoing }
        let currentIDs = Set(incoming.map(\.notificationID))
        let newOnes = didSeed ? incoming.filter { !seenIDs.contains($0.notificationID) } : []
        markSeen(currentIDs)
        if !didSeed {
            didSeed = true
            UserDefaults.standard.set(true, forKey: Self.seenSeededKey)
            return
        }
        for item in newOnes {
            attentionStore.recordIncomingSMS(messageID: item.forwardingID, threadID: item.threadID)
        }
        await notifyIncoming(newOnes, generation: generation)
    }

    private func notifyIncoming(_ newOnes: [SMSItem], generation: UInt64) async {
        guard isCurrentRefresh(generation) else { return }
        let toNotify = newOnes.filter { !attentionStore.isViewingSMS(threadID: $0.threadID) }
        guard !toNotify.isEmpty, notificationsEnabled else { return }
        // 未授权时请求授权（首次收到短信时弹出系统授权框）
        guard await ensureAuthorization(), isCurrentRefresh(generation) else { return }
        // This list is stable on the main actor for the synchronous loop below.
        // Index it once so large inboxes do not rescan and re-hash every item
        // for each notification.
        let activeNotificationIDs = Set(items.map(\.notificationID))
        for item in toNotify.sorted(by: { $0.timestamp < $1.timestamp }) {
            guard !attentionStore.isViewingSMS(threadID: item.threadID),
                  activeNotificationIDs.contains(item.notificationID) else { continue }
            postNotification(for: item)
        }
    }

    private func markSeen(_ ids: Set<String>) {
        for id in ids where !seenIDs.contains(id) {
            seenIDs.insert(id)
            seenOrder.append(id)
        }
        // 只保留最近 2000 条记录，避免无限增长
        let tail = seenOrder.filter { !ids.contains($0) }.suffix(2000)
        seenOrder = Array(tail) + seenOrder.filter { ids.contains($0) }
        seenIDs = Set(seenOrder)
        UserDefaults.standard.set(Array(seenOrder), forKey: Self.seenKey)
    }

    private func postNotification(for item: SMSItem) {
        let content = UNMutableNotificationContent()
        let privacyMaskEnabled = UserDefaults.standard.object(
            forKey: AppPreferenceKeys.privacyMaskEnabled) as? Bool ?? true
        content.title = privacyMaskEnabled ? "新短信" : (item.sender ?? "新短信")
        content.body = privacyMaskEnabled
            ? "内容已隐藏，请打开 Switch 查看"
            : String((item.content ?? "").prefix(300))
        content.sound = .default
        content.userInfo = ["source": "device", "sender": item.sender ?? "",
                            "device_id": item.lineID, "message_id": item.backendID ?? item.notificationID]
        if let subscription = item.subscription { content.userInfo["subscription_id"] = subscription }
        let request = UNNotificationRequest(
            identifier: "sms-" + UUID().uuidString,
            content: content,
            trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    // MARK: - 通知点击

    /// 点击通知后打开短信页（由 AppDelegate 调用）
    func requestOpenSMS(sender: String?, lineID: String? = nil, messageID: String? = nil, subscriptionID: String? = nil) {
        pendingOpenLineID = lineID
        pendingOpenMessageID = messageID
        pendingOpenSubscriptionID = SMSConversationIdentity.normalizedSubscription(subscriptionID)
        pendingOpenSender = (sender?.isEmpty ?? true) ? nil : sender
    }

    /// 通知已消费：短信页处理完打开请求后调用
    func consumeOpenRequest() {
        pendingOpenSender = nil
        pendingOpenLineID = nil
        pendingOpenMessageID = nil
        pendingOpenSubscriptionID = nil
    }

    // MARK: - 删除

    func deleteMessage(_ item: SMSItem, refreshAfter: Bool = true) async throws -> Bool {
        let lifecycle = lifecycleGeneration
        let request: SMSDeleteRequest
        if item.isFromModule {
            request = SMSDeleteRequest(
                id: item.backendID,
                storage: item.moduleStorage ?? "ME",
                index: item.moduleIndex ?? 0,
                deletionToken: item.deletionToken)
        } else {
            request = SMSDeleteRequest(
                id: item.backendID,
                sender: item.sender,
                content: item.content,
                timestamp: item.timestamp)
        }
        let result: SMSDeleteResult = try await APIClient().send(
            "api/sms/delete",
            body: request)
        if result.deleted {
            guard lifecycle == lifecycleGeneration else { return result.deleted }
            let force = pendingForcedRefresh
            invalidateRefresh()
            items.removeAll { $0.id == item.id }
            attentionStore.forgetSMS(messageID: item.forwardingID)
            if refreshAfter { refresh(force: force) }
        }
        return result.deleted
    }

    /// Delete the reviewed snapshot with stable message identities. The legacy
    /// sender-wide endpoint spans devices and cannot safely delete one thread.
    func deleteConversation(_ snapshot: [SMSItem]) async throws -> Int {
        let lifecycle = lifecycleGeneration
        var count = 0
        defer { if lifecycle == lifecycleGeneration { refresh() } }
        for item in snapshot {
            guard lifecycle == lifecycleGeneration else { throw URLError(.cancelled) }
            if try await deleteMessage(item, refreshAfter: false) { count += 1 }
        }
        return count
    }

    func deleteMessages(from sender: String) async throws -> Int {
        let trimmed = sender.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return 0 }
        let lifecycle = lifecycleGeneration
        let selectedIDs = Set(items.filter {
            $0.sender?.trimmingCharacters(in: .whitespacesAndNewlines) == trimmed
        }.map(\.id))
        let result: SMSDeleteSenderResult = try await APIClient().send(
            "api/sms/delete-sender",
            body: SMSDeleteSenderRequest(sender: trimmed))
        if result.deletedCount > 0 {
            guard lifecycle == lifecycleGeneration else { return result.deletedCount }
            let force = pendingForcedRefresh
            invalidateRefresh()
            items.removeAll { selectedIDs.contains($0.id) }
            // The legacy delete-sender API removes every device line for this peer.
            // New SMS-center thread deletion uses precise per-message requests.
            attentionStore.markSMSThreadViewed(SMSConversationIdentity.device(lineID: "dji", peer: trimmed))
            attentionStore.markSMSThreadViewed(SMSConversationIdentity.device(lineID: "reader", peer: trimmed))
            refresh(force: force)
        }
        return result.deletedCount
    }

    // MARK: - 调试：模拟通知

    /// 模拟一条新短信通知（走与真实短信相同的发送路径），返回结果描述
    func simulateNotification() async -> String {
        guard await ensureAuthorization() else {
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            if settings.authorizationStatus == .denied {
                return "通知权限已被拒绝，请在系统设置中开启"
            }
            return "通知权限未授予"
        }
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        // 已授权但横幅被关闭（静默）时提醒用户
        if settings.alertSetting == .disabled {
            return "通知已发送到通知中心，但横幅/提示被系统关闭，请点击\"打开通知设置\"将样式改为横幅"
        }
        let item = SMSItem(
            backendID: nil,
            sender: "+8613800000000",
            content: "这是一条模拟短信，用于测试通知功能。",
            code: nil,
            timestamp: Date(),
            moduleStorage: nil,
            moduleIndex: nil,
            archived: nil,
            direction: nil,
            device: nil)
        postNotification(for: item)
        return "已发送模拟通知"
    }
}
