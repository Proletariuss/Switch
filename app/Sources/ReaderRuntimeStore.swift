import Combine
import Foundation

/// Owns the complete reader identity and Wi-Fi Calling lifecycle.
///
/// A monotonically increasing generation rejects replies that belong to an
/// older reader or eSIM profile. User-entered proxy and ePDG settings survive
/// disconnects; every value derived from the physical card is cleared as one
/// unit when the reader disappears.
@MainActor
final class ReaderRuntimeStore: ObservableObject {
    @Published private(set) var state = ReaderRuntimeState.initial
    var revision: UInt64 { state.revision }
    var readers: [String] { state.readers }
    var listLoaded: Bool { state.listLoaded }
    var connected: Bool { state.connected }
    var name: String? { state.name }
    var usimInfo: USIMInfo? { state.usimInfo }
    /// Presentation may retain the selected reader's capability skeleton,
    /// but no SIM-dependent service is ready until a fresh identity exists.
    var identityPending: Bool { connected && usimInfo == nil }
    var profileOperatorName: String? { state.profileOperatorName }
    var epdgFQDN: String? { state.epdgFQDN }
    var carrierConfigError: String? { state.carrierConfigError }
    var hasESIM: Bool { state.hasESIM }
    var cardType: String? { state.cardType }
    var error: String? { state.error }
    var actionInFlight: Bool { state.actionInFlight }
    var voWiFiStatus: ReaderVoWiFiStatus? { state.voWiFiStatus }
    var voWiFiLoaded: Bool { state.voWiFiLoaded }
    var voWiFiActionInFlight: Bool { state.voWiFiActionInFlight }
    @Published var proxyIP: String = UserDefaults.standard.string(forKey: "readerVoWiFiProxyIP") ?? "" {
        didSet { UserDefaults.standard.set(proxyIP, forKey: "readerVoWiFiProxyIP") }
    }
    @Published var proxyPort: String = UserDefaults.standard.string(forKey: "readerVoWiFiProxyPort") ?? "" {
        didSet { UserDefaults.standard.set(proxyPort, forKey: "readerVoWiFiProxyPort") }
    }
    @Published var customEPDG: String = UserDefaults.standard.string(forKey: "readerVoWiFiEPDG") ?? "" {
        didSet { UserDefaults.standard.set(customEPDG, forKey: "readerVoWiFiEPDG") }
    }
    @Published var customEPDGPort: String = UserDefaults.standard.string(forKey: "readerVoWiFiEPDGPort") ?? "" {
        didSet { UserDefaults.standard.set(customEPDGPort, forKey: "readerVoWiFiEPDGPort") }
    }
    @Published var proxyCheckResult: ProxyCheckResult?
    @Published var proxyChecking = false
    @Published var epdgError: String?

    private(set) var generation = 0
    private(set) var readerSessionGeneration: UInt64?
    private var requiresReaderGeneration = false
    private var observedReaderGeneration: UInt64?
    private var observedInventoryRevision: UInt64?
    private var observedInstanceID: String?
    private(set) var listRefreshInFlight = false
    private(set) var fallbackRecoveryInFlight = false
    private(set) var voWiFiRefreshInFlight = false
    private(set) var lastVoWiFiHealthRefreshAt: Date?
    // A single status GET can fail while the local backend is restarting.
    // Do not immediately turn a healthy card red, but never leave a stale
    // ready state visible after repeated failures either.
    private var consecutiveVoWiFiStatusReadFailures = 0

    var onToast: ((String, Bool, String?) -> Void)?

    private let checkpoints: VoWiFiRequestCheckpointStore
    private let lifecycleCheckpoints: ReaderLifecycleMutationCheckpointStore
    private var pendingCheckpoint: VoWiFiRequestCheckpoint?
    private var checkpointFailure = false
    private var deferredTerminal: VoWiFiOperation?
    private let backend: BackendProcess
    private var voWiFiPollTask: Task<Void, Never>?
    private var voWiFiOwnerOperation: VoWiFiOperation?
    private var stopTargetCache: VoWiFiStopTarget?
    private var activeStopTarget: VoWiFiStopTarget?
    private var temporaryStopMessage: String?
    private var proxyCheckTask: Task<Void, Never>?
    private var listRequestRevision: UInt64 = 0
    private var voWiFiOperationRevision: UInt64 = 0
    private var voWiFiStatusRevision: UInt64 = 0
    private var proxyCheckRevision: UInt64 = 0
    // Displayed identity is withdrawn during an uncertain observation. Keep
    // the last verified binding separately so recovery of that same card
    // cannot abandon an outstanding start/stop operation.
    private struct CardIdentity: Equatable {
        let iccid: String?
        let imsi: String?
        init(_ info: USIMInfo?) {
            iccid = info?.iccid
            imsi = info?.imsi
        }
    }
    private var verifiedCardIdentity = CardIdentity(nil)
    private var cardInterruptionConfirmed = false

    init(
        backend: BackendProcess,
        checkpointRoot: URL? = nil,
        lifecycleCheckpoints: ReaderLifecycleMutationCheckpointStore = ReaderLifecycleMutationCheckpointStore()
    ) {
        self.checkpoints = VoWiFiRequestCheckpointStore(scope: "reader", root: checkpointRoot)
        self.lifecycleCheckpoints = lifecycleCheckpoints
        self.backend = backend
        resumeVoWiFiCheckpoint()
    }

    var operatorDisplayName: String {
        if let value = usimInfo?.operatorName?.trimmingCharacters(in: .whitespacesAndNewlines),
           !value.isEmpty {
            return value
        }
        if let profileOperatorName, !profileOperatorName.isEmpty { return profileOperatorName }
        return usimInfo?.operatorDisplayName ?? "未知运营商"
    }

    func markVoWiFiHealthRefresh() {
        lastVoWiFiHealthRefreshAt = Date()
    }

    func reset() {
        stopTargetCache = nil
        activeStopTarget = nil
        temporaryStopMessage = nil
        listRequestRevision &+= 1
        listRefreshInFlight = false
        fallbackRecoveryInFlight = false
        proxyCheckRevision &+= 1
        voWiFiPollTask?.cancel()
        voWiFiPollTask = nil
        proxyCheckTask?.cancel()
        proxyCheckTask = nil
        proxyCheckResult = nil
        proxyChecking = false
        epdgError = nil
        consecutiveVoWiFiStatusReadFailures = 0
        clearObservedLifecycleProof()
        clearRuntimeState(clearList: true)
    }

    func applySnapshot(
        _ snapshot: ReaderDeviceSnapshot,
        issues: [DeviceSnapshotIssue],
        expectedGeneration: Int,
        autoConnect: Bool,
        instanceID: String? = nil
    ) {
        guard !actionInFlight, expectedGeneration == generation else { return }
        let voWiFiIssue = issues.first { $0.endpoint == "/api/reader/vowifi" }
        if let issue = issues.first(where: {
            $0.endpoint == "/api/reader/list"
                || $0.endpoint == "/api/v2/reader-discovery"
                || $0.endpoint == "/api/reader/info"
        }) {
            invalidatePendingVoWiFiStatusReads()
            commit { state in
                state.listLoaded = true
                // The aggregate snapshot could not verify reader identity.
                // Do not leave a previously ready IMS/VoWiFi state visible;
                // the start/stop operation itself must keep running so its
                // completion path can reconcile the real backend state.
                Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                Self.clearVoWiFiSnapshotFields(&state)
                state.error = "读取读卡器快照失败：\(issue.message)"
            }
            return
        }
        if let revision = snapshot.inventoryRevision, revision > 0,
           let generation = snapshot.readerGeneration,
           let instanceID = instanceID?.trimmingCharacters(in: .whitespacesAndNewlines),
           !instanceID.isEmpty {
            observedInventoryRevision = revision
            observedReaderGeneration = generation
            observedInstanceID = instanceID
        }

        guard let selected = snapshot.selected else {
            if connected, name != nil {
                // Native discovery can miss one scan while the retained owner
                // still holds the selected card. Verify native ownership
                // before clearing it or auto-selecting another
                // reader. The confirmed hotplug monitor retires a real loss.
                refreshAfterAggregateFailure(autoConnect: false)
                return
            }
            clearRuntimeState(
                keepingReaders: snapshot.readers,
                listLoaded: true)
            if autoConnect, let proposal = snapshot.autoConnect { connect(proposal.reader, automatic: proposal) }
            return
        }

        let selectionChanged = name != selected
        if let info = snapshot.info {
            applyInfo(
                info,
                readers: snapshot.readers,
                listLoaded: true,
                clearVoWiFi: selectionChanged)
        } else {
            let retainCapabilities = connected && !selectionChanged
            if selectionChanged {
                invalidateVoWiFiOperationForCardTransition()
            } else {
                invalidatePendingVoWiFiStatusReads()
            }
            commit { state in
                Self.clearCardIdentityFields(&state, preservingCapabilities: retainCapabilities)
                if selectionChanged {
                    Self.clearVoWiFiFields(&state)
                } else {
                    // Keep ownership of an in-flight stop/start, never its
                    // old ready snapshot while SIM identity is unavailable.
                    Self.clearVoWiFiSnapshotFields(&state)
                }
                state.readers = snapshot.readers
                state.listLoaded = true
                state.name = selected
                state.connected = true
                state.error = "读卡器已选择，但身份快照尚未就绪"
            }
        }
        if let status = snapshot.vowifi, !voWiFiActionInFlight, usimInfo != nil {
            publishVoWiFiStatus(status)
        } else if voWiFiIssue != nil, !voWiFiActionInFlight {
            invalidatePendingVoWiFiStatusReads()
            commit { state in
                state.voWiFiStatus = nil
                state.voWiFiLoaded = true
            }
        }
    }

    func refreshList(
        autoConnect: Bool = true,
        refreshConnectedInfo: Bool = true,
        refreshVoWiFiAfterIdentity: Bool = false
    ) {
        guard case .running = backend.state, !actionInFlight, !listRefreshInFlight else { return }
        let expectedGeneration = generation
        listRequestRevision &+= 1
        let requestRevision = listRequestRevision
        let initialOperationRevision = voWiFiOperationRevision
        let initialStatusRevision = voWiFiStatusRevision
        listRefreshInFlight = true
        Task {
            var expectedOperationRevision = initialOperationRevision
            var expectedStatusRevision = initialStatusRevision
            var identityVerified = false
            defer {
                if requestRevision == listRequestRevision {
                    listRefreshInFlight = false
                    if refreshVoWiFiAfterIdentity { fallbackRecoveryInFlight = false }
                }
            }
            do {
                let response: ReaderListResponse
                var retainedInfo: ReaderInfoResponse?
                var inventoryUnavailable = false
                do {
                    response = try await APIClient(timeoutInterval: 10).get("api/reader/list")
                } catch {
                    // A PC/SC inventory failure cannot starve an independent
                    // retained BLE/card owner whose information is readable.
                    guard expectedGeneration == generation,
                          requestRevision == listRequestRevision,
                          !actionInFlight,
                          expectedOperationRevision == voWiFiOperationRevision,
                          expectedStatusRevision == voWiFiStatusRevision else { return }
                    guard connected, name != nil else { throw error }
                    invalidatePendingVoWiFiStatusReads()
                    expectedStatusRevision = voWiFiStatusRevision
                    commit { state in
                        Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                        Self.clearVoWiFiSnapshotFields(&state)
                    }
                    let info: ReaderInfoResponse = try await APIClient(timeoutInterval: 10).get("api/reader/info")
                    guard expectedGeneration == generation,
                          requestRevision == listRequestRevision,
                          !actionInFlight,
                          expectedOperationRevision == voWiFiOperationRevision,
                          expectedStatusRevision == voWiFiStatusRevision else { return }
                    if info.status.connected,
                       info.status.readerName?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false {
                        setError("无法确认已选读卡器；暂不自动切换")
                        return
                    }
                    retainedInfo = info
                    inventoryUnavailable = true
                    response = ReaderListResponse(readers: readers,
                        selected: info.status.connected ? info.status.readerName : nil, devices: nil)
                }
                guard expectedGeneration == generation,
                      requestRevision == listRequestRevision,
                      !actionInFlight,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                if let revision = response.inventoryRevision, revision > 0,
                   let readerGeneration = response.readerGeneration,
                   let instanceID = response.instanceID?.trimmingCharacters(in: .whitespacesAndNewlines),
                   !instanceID.isEmpty {
                    observedInventoryRevision = revision
                    observedReaderGeneration = readerGeneration
                    observedInstanceID = instanceID
                }
                var selected = response.selected
                if selected == nil, connected, name != nil, !inventoryUnavailable {
                    // Enumeration is not a physical-close receipt. Verify the
                    // retained owner before replacing a locally selected card.
                    invalidatePendingVoWiFiStatusReads()
                    expectedStatusRevision = voWiFiStatusRevision
                    commit { state in
                        Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                        Self.clearVoWiFiSnapshotFields(&state)
                        state.error = "扫描未确认已选读卡器，正在核对连接状态"
                    }
                    let info: ReaderInfoResponse = try await APIClient(timeoutInterval: 10)
                        .get("api/reader/info")
                    guard expectedGeneration == generation,
                          requestRevision == listRequestRevision,
                          !actionInFlight,
                          expectedOperationRevision == voWiFiOperationRevision,
                          expectedStatusRevision == voWiFiStatusRevision else { return }
                    if info.status.connected {
                        guard let retainedName = info.status.readerName,
                              !retainedName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                            invalidatePendingVoWiFiStatusReads()
                            commit { state in
                                Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                                Self.clearVoWiFiSnapshotFields(&state)
                                state.error = "无法确认已选读卡器；暂不自动切换"
                            }
                            return
                        }
                        selected = retainedName
                        retainedInfo = info
                    }
                }
                if let selected {
                    let selectionChanged = name != selected
                    if selectionChanged {
                        // A list-only reader transition must fence pending
                        // direct VoWiFi reads just like an aggregate snapshot
                        // or profile identity transition does.
                        invalidateVoWiFiOperationForCardTransition()
                        consecutiveVoWiFiStatusReadFailures = 0
                        expectedOperationRevision = voWiFiOperationRevision
                        expectedStatusRevision = voWiFiStatusRevision
                    }
                    commit { state in
                        if selectionChanged { Self.clearDerivedFields(&state) }
                        state.readers = response.readers
                        state.listLoaded = true
                        state.error = nil
                        state.name = selected
                        state.connected = true
                    }
                    if refreshVoWiFiAfterIdentity || retainedInfo != nil {
                        // A failed aggregate cannot safely publish VoWiFi
                        // status against the prior profile. Read the current
                        // card identity first, even when reader name is the
                        // same, then read status without waiting for module
                        // health/status compatibility calls.
                        let info: ReaderInfoResponse
                        if let retainedInfo {
                            info = retainedInfo
                        } else {
                            info = try await APIClient(timeoutInterval: 10).get("api/reader/info")
                        }
                        guard expectedGeneration == generation,
                              requestRevision == listRequestRevision,
                              !actionInFlight,
                              expectedOperationRevision == voWiFiOperationRevision,
                              expectedStatusRevision == voWiFiStatusRevision else { return }
                        applyInfo(info, readers: response.readers, listLoaded: true,
                                  clearVoWiFi: selectionChanged)
                        identityVerified = true
                        guard expectedGeneration == generation, connected else { return }
                        expectedOperationRevision = voWiFiOperationRevision
                        expectedStatusRevision = voWiFiStatusRevision

                        if inventoryUnavailable, self.error == nil {
                            setError("读卡器列表暂不可用，已核实当前连接")
                        }
                        guard refreshVoWiFiAfterIdentity || inventoryUnavailable,
                              !voWiFiActionInFlight else { return }
                        let status: ReaderVoWiFiStatus = try await APIClient(timeoutInterval: 10)
                            .get("api/reader/vowifi")
                        guard expectedGeneration == generation,
                              requestRevision == listRequestRevision,
                              !actionInFlight,
                              expectedOperationRevision == voWiFiOperationRevision,
                              expectedStatusRevision == voWiFiStatusRevision else { return }
                        publishDirectVoWiFiStatus(status)
                    } else if refreshConnectedInfo || selectionChanged {
                        refreshInfo()
                    }
                } else {
                    clearRuntimeState(
                        keepingReaders: response.readers,
                        listLoaded: true)
                }
                if selected == nil, autoConnect, let proposal = response.autoConnect {
                    connect(proposal.reader, automatic: proposal)
                }
            } catch {
                guard expectedGeneration == generation,
                      requestRevision == listRequestRevision,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                invalidatePendingVoWiFiStatusReads()
                commit { state in
                    // A later VoWiFi status failure does not revoke the fresh
                    // card identity just verified by the independent owner.
                    if !identityVerified {
                        Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                    }
                    Self.clearVoWiFiSnapshotFields(&state)
                    state.listLoaded = true
                    state.error = refreshVoWiFiAfterIdentity
                        ? "回退读取读卡器身份与 VoWiFi 状态失败：\(error.localizedDescription)"
                        : "枚举读卡器失败：\(error.localizedDescription)"
                }
            }
        }
    }

    /// Rust aggregate failure recovery is independent from module reads.
    /// Withdraw stale readiness immediately; the bounded list→info→status
    /// sequence is the only path allowed to republish it.
    func refreshAfterAggregateFailure(autoConnect: Bool = true) {
        guard case .running = backend.state, !actionInFlight else { return }
        if fallbackRecoveryInFlight && voWiFiStatus == nil {
            // Another failed aggregate must not restart a bounded recovery
            // faster than list→info→status can finish. The first attempt has
            // already withdrawn readiness and remains authoritative.
            return
        }
        invalidatePendingVoWiFiStatusReads()
        commit { state in Self.clearVoWiFiSnapshotFields(&state) }
        // A manual list read might already be pending. Supersede it so its
        // reply cannot race with this identity-bound recovery sequence.
        listRequestRevision &+= 1
        listRefreshInFlight = false
        fallbackRecoveryInFlight = true
        refreshList(
            autoConnect: autoConnect,
            refreshConnectedInfo: true,
            refreshVoWiFiAfterIdentity: true)
    }

    func connect(_ reader: String, automatic: ReaderAutoConnect? = nil) {
        guard !actionInFlight else { return }
        guard !voWiFiActionInFlight else {
            toast("Wi-Fi Calling 操作正在进行，请稍后切换读卡器", false, "读卡器")
            return
        }
        let proofReaderGeneration = automatic?.readerGeneration ?? observedReaderGeneration
        let proofInventoryRevision = automatic?.inventoryRevision ?? observedInventoryRevision
        let proofInstanceID = automatic?.instanceID ?? observedInstanceID
        guard let proofReaderGeneration, let proofInventoryRevision,
              let proofInstanceID, proofInventoryRevision > 0 else {
            setError("读卡器清单身份尚未确认，请先刷新后再连接")
            toast("请先刷新读卡器清单", false, "读卡器")
            refreshList(autoConnect: false, refreshConnectedInfo: false)
            return
        }
        guard let lifecycleCheckpoint = lifecycleCheckpoints.begin(
            action: "connect", reader: reader,
            readerGeneration: proofReaderGeneration,
            inventoryRevision: proofInventoryRevision,
            instanceID: proofInstanceID,
            autoConnect: automatic) else {
            setError("读卡器连接请求记录损坏或仍有旧操作待确认，未发送新请求")
            toast("读卡器操作记录待确认", false, "读卡器")
            return
        }
        generation &+= 1
        let expectedGeneration = generation
        invalidateVoWiFiOperationForCardTransition()
        consecutiveVoWiFiStatusReadFailures = 0
        commit { state in
            state.actionInFlight = true
            state.error = nil
            Self.clearVoWiFiSnapshotFields(&state)
        }
        Task {
            defer { if expectedGeneration == generation && actionInFlight { setActionInFlight(false) } }
            do {
                let response: ReaderInfoResponse = try await APIClient(timeoutInterval: 30)
                    .send("api/reader/connect", body: ReaderConnectRequest(
                        clientRequestID: lifecycleCheckpoint.clientRequestID,
                        reader: lifecycleCheckpoint.reader,
                        readerGeneration: lifecycleCheckpoint.readerGeneration,
                        inventoryRevision: lifecycleCheckpoint.inventoryRevision,
                        instanceID: lifecycleCheckpoint.instanceID,
                        autoConnect: lifecycleCheckpoint.autoConnect))
                guard expectedGeneration == generation else { return }
                guard lifecycleCheckpoints.clear(lifecycleCheckpoint) else {
                    setError("读卡器已响应，但本机请求记录清除未确认；已保留操作围栏")
                    toast("读卡器操作记录待确认", false, "读卡器")
                    return
                }
                applyInfo(response)
                toast("读卡器已连接", true, "读卡器")
            } catch {
                guard expectedGeneration == generation else { return }
                if Self.readerLifecycleRequestWasNotSubmitted(error) {
                    _ = lifecycleCheckpoints.clear(lifecycleCheckpoint)
                }
                setError("连接读卡器失败：\(error.localizedDescription)")
                toast("连接失败：\(error.localizedDescription)", false, "读卡器")
                setActionInFlight(false)
                refreshVoWiFi(force: true)
            }
        }
    }

    func disconnect() {
        guard !actionInFlight else { return }
        guard !voWiFiActionInFlight else {
            toast("Wi-Fi Calling 操作正在进行，请稍后断开读卡器", false, "读卡器")
            return
        }
        guard let reader = name,
              let proofReaderGeneration = readerSessionGeneration ?? observedReaderGeneration,
              let proofInventoryRevision = observedInventoryRevision,
              let proofInstanceID = observedInstanceID,
              proofInventoryRevision > 0 else {
            setError("当前读卡器身份尚未确认，请先刷新后再断开")
            toast("请先刷新读卡器状态", false, "读卡器")
            refreshList(autoConnect: false, refreshConnectedInfo: true)
            return
        }
        guard let lifecycleCheckpoint = lifecycleCheckpoints.begin(
            action: "disconnect", reader: reader,
            readerGeneration: proofReaderGeneration,
            inventoryRevision: proofInventoryRevision,
            instanceID: proofInstanceID,
            autoConnect: nil) else {
            setError("读卡器断开请求记录损坏或仍有旧操作待确认，未发送新请求")
            toast("读卡器操作记录待确认", false, "读卡器")
            return
        }
        generation &+= 1
        let expectedGeneration = generation
        invalidateVoWiFiOperationForCardTransition()
        consecutiveVoWiFiStatusReadFailures = 0
        commit { state in
            state.actionInFlight = true
            Self.clearVoWiFiSnapshotFields(&state)
        }
        Task {
            defer { if expectedGeneration == generation && actionInFlight { setActionInFlight(false) } }
            do {
                let _: EmptyResponse = try await APIClient(timeoutInterval: 15)
                    .send("api/reader/disconnect", body: ReaderDisconnectRequest(
                        clientRequestID: lifecycleCheckpoint.clientRequestID,
                        reader: lifecycleCheckpoint.reader,
                        readerGeneration: lifecycleCheckpoint.readerGeneration,
                        inventoryRevision: lifecycleCheckpoint.inventoryRevision,
                        instanceID: lifecycleCheckpoint.instanceID), as: EmptyResponse.self)
                guard expectedGeneration == generation else { return }
                guard lifecycleCheckpoints.clear(lifecycleCheckpoint) else {
                    setError("读卡器已响应，但本机请求记录清除未确认；已保留操作围栏")
                    toast("读卡器操作记录待确认", false, "读卡器")
                    return
                }
                clearObservedLifecycleProof()
                clearRuntimeState()
                toast("读卡器已断开", true, "读卡器")
            } catch {
                guard expectedGeneration == generation else { return }
                if Self.readerLifecycleRequestWasNotSubmitted(error) {
                    _ = lifecycleCheckpoints.clear(lifecycleCheckpoint)
                }
                setError("断开读卡器失败：\(error.localizedDescription)")
                toast("断开失败：\(error.localizedDescription)", false, "读卡器")
                setActionInFlight(false)
                refreshVoWiFi(force: true)
            }
        }
    }

    func refreshInfo() {
        guard !actionInFlight else { return }
        let expectedGeneration = generation
        setActionInFlight(true)
        Task {
            defer { if expectedGeneration == generation { setActionInFlight(false) } }
            do {
                let response: ReaderInfoResponse = try await APIClient(timeoutInterval: 30)
                    .get("api/reader/info")
                guard expectedGeneration == generation else { return }
                applyInfo(response)
            } catch {
                guard expectedGeneration == generation else { return }
                invalidatePendingVoWiFiStatusReads()
                commit { state in
                    Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                    Self.clearVoWiFiSnapshotFields(&state)
                    state.error = "获取读卡器信息失败：\(error.localizedDescription)"
                }
            }
        }
    }

    func refreshSIM() {
        guard !actionInFlight else { return }
        let expectedGeneration = generation
        setActionInFlight(true)
        Task {
            defer { if expectedGeneration == generation { setActionInFlight(false) } }
            do {
                let response: ReaderInfoResponse = try await APIClient(timeoutInterval: 30)
                    .send("api/reader/refresh")
                guard expectedGeneration == generation else { return }
                applyInfo(response)
                toast("SIM 卡信息已刷新", true, "读卡器")
            } catch {
                guard expectedGeneration == generation else { return }
                setError("刷新 SIM 失败：\(error.localizedDescription)")
                toast("刷新失败：\(error.localizedDescription)", false, "读卡器")
            }
        }
    }

    func applyInfo(
        _ response: ReaderInfoResponse,
        readers: [String]? = nil,
        listLoaded: Bool? = nil,
        clearVoWiFi: Bool = false
    ) {
        guard response.status.connected else {
            clearRuntimeState(
                keepingReaders: readers ?? self.readers,
                listLoaded: listLoaded ?? self.listLoaded)
            return
        }
        let identityUnavailable = response.status.identityObservationUnavailable
        let nextIdentity = identityUnavailable ? nil : response.usimInfo
        let carrier = nextIdentity?.operatorName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let readerChanged = name != response.status.readerName
        // A same-reader USIM refresh can briefly have no capability result.
        // Retain only the known card-type skeleton, not the old SIM identity.
        // Explicit removal/type or a newly read SIM always replaces it, so a
        // physical SIM inserted into the same reader cannot inherit eUICC UI.
        let retainCapabilities = connected && !readerChanged && response.status.hasCard
            && !identityUnavailable && nextIdentity == nil
            && (response.status.cardType?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
        let nextCardIdentity = CardIdentity(nextIdentity)
        let profileChanged = verifiedCardIdentity != nextCardIdentity
        let nextReaderGeneration = response.status.readerGeneration.flatMap { $0 == 0 ? nil : $0 }
        let readerGenerationChanged = readerSessionGeneration != nextReaderGeneration
        let temporaryIdentityGap = identityUnavailable
            && !response.status.hasConfirmedCardInterruption
            && !readerChanged && !readerGenerationChanged
        let newCardInterruption = response.status.hasConfirmedCardInterruption
            && !cardInterruptionConfirmed
        let identityTransition = clearVoWiFi || readerChanged || readerGenerationChanged
            || newCardInterruption
            || (profileChanged && !temporaryIdentityGap)
        if identityTransition {
            invalidateVoWiFiOperationForCardTransition()
            consecutiveVoWiFiStatusReadFailures = 0
        } else if identityUnavailable {
            invalidatePendingVoWiFiStatusReads()
        }
        if !temporaryIdentityGap { verifiedCardIdentity = nextCardIdentity }
        if response.status.hasConfirmedCardInterruption {
            cardInterruptionConfirmed = true
        } else if !identityUnavailable {
            cardInterruptionConfirmed = false
        }
        readerSessionGeneration = nextReaderGeneration
        if let nextReaderGeneration { observedReaderGeneration = nextReaderGeneration }
        if nextReaderGeneration != nil { requiresReaderGeneration = true }
        commit { state in
            if let readers { state.readers = readers }
            if let listLoaded { state.listLoaded = listLoaded }
            state.connected = true
            state.name = response.status.readerName
            state.hasESIM = !response.status.hasConfirmedCardInterruption
                && (response.status.hasESIM || (retainCapabilities && state.hasESIM))
            state.cardType = response.status.hasConfirmedCardInterruption ? nil
                : (retainCapabilities ? state.cardType : response.status.cardType)
            state.usimInfo = nextIdentity
            state.profileOperatorName = carrier.isEmpty ? nil : carrier
            state.epdgFQDN = nextIdentity == nil ? nil : response.epdgFqdn
            state.carrierConfigError = nextIdentity == nil ? nil : response.carrierConfigError
            state.error = response.status.cardObservationMessage
            if identityTransition {
                state.voWiFiStatus = nil
                state.voWiFiLoaded = false
                state.voWiFiActionInFlight = false
            } else if identityUnavailable {
                // Withdraw stale readiness without marking an in-flight
                // native start/stop operation complete or cancelling it.
                Self.clearVoWiFiSnapshotFields(&state)
            }
        }
    }

    func clearRuntimeState(
        clearList: Bool = false,
        keepingReaders: [String]? = nil,
        listLoaded: Bool? = nil
    ) {
        let targetReaders = clearList ? [] : (keepingReaders ?? state.readers)
        let targetListLoaded = clearList ? false : (listLoaded ?? state.listLoaded)
        let hadRuntimeState = connected || name != nil || usimInfo != nil ||
            profileOperatorName != nil || epdgFQDN != nil || hasESIM || cardType != nil ||
            actionInFlight || voWiFiStatus != nil || voWiFiLoaded ||
            voWiFiActionInFlight || voWiFiRefreshInFlight
        let listChanged = state.readers != targetReaders || state.listLoaded != targetListLoaded
        guard hadRuntimeState || clearList || listChanged else { return }
        if hadRuntimeState || clearList { generation &+= 1 }
        invalidateVoWiFiOperationForCardTransition()
        readerSessionGeneration = nil
        requiresReaderGeneration = false
        consecutiveVoWiFiStatusReadFailures = 0
        lastVoWiFiHealthRefreshAt = nil
        epdgError = nil
        commit { state in
            Self.clearDerivedFields(&state)
            state.readers = targetReaders
            state.listLoaded = targetListLoaded
        }
    }

    private func clearObservedLifecycleProof() {
        observedReaderGeneration = nil
        observedInventoryRevision = nil
        observedInstanceID = nil
    }

    /// Rust validates stale bindings before writing a submitted receipt.  That
    /// one rejection therefore proves no device effect occurred and may
    /// release the local checkpoint.  Transport/server/ID-conflict failures
    /// retain it because they cannot prove that an earlier submission did not
    /// cross the mutation boundary.
    private static func readerLifecycleRequestWasNotSubmitted(_ error: any Error) -> Bool {
        readerLifecycleFailureWasDefinitelyNotSubmitted(error)
    }

    func updateProfileOperatorName(_ value: String?) {
        let normalized = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        commit { $0.profileOperatorName = normalized.isEmpty ? nil : normalized }
    }

    func synchronizeAfterProfileSwitch(_ snapshot: ReaderInfoResponse? = nil) {
        generation &+= 1
        let expectedGeneration = generation
        invalidateVoWiFiOperationForCardTransition()
        if let snapshot {
            applyInfo(snapshot, clearVoWiFi: true)
        } else {
            // Activation is being confirmed. Withdraw every field derived
            // from the old profile before I/O while retaining the same
            // physical reader's known capability for a stable page skeleton.
            commit { state in
                Self.clearCardIdentityFields(&state, preservingCapabilities: true)
                Self.clearVoWiFiFields(&state)
            }
        }
        let initialOperationRevision = voWiFiOperationRevision
        let initialStatusRevision = voWiFiStatusRevision
        Task {
            var expectedOperationRevision = initialOperationRevision
            var expectedStatusRevision = initialStatusRevision
            do {
                guard expectedGeneration == generation,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                if snapshot == nil {
                    let info: ReaderInfoResponse = try await APIClient(timeoutInterval: 45)
                        .send("api/reader/refresh")
                    guard expectedGeneration == generation,
                          expectedOperationRevision == voWiFiOperationRevision,
                          expectedStatusRevision == voWiFiStatusRevision else { return }
                    applyInfo(info)
                    guard expectedGeneration == generation else { return }
                    // Applying the freshly read card may itself invalidate
                    // the prior VoWiFi operation revision. Bind the next GET
                    // to that new card identity, not to the old revision.
                    expectedOperationRevision = voWiFiOperationRevision
                    expectedStatusRevision = voWiFiStatusRevision
                }
                let status: ReaderVoWiFiStatus = try await APIClient(timeoutInterval: 15)
                    .get("api/reader/vowifi")
                guard expectedGeneration == generation,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                publishDirectVoWiFiStatus(status)
            } catch {
                guard expectedGeneration == generation,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                commit { state in
                    state.voWiFiLoaded = true
                    state.error = "切换后同步卡片信息失败：\(error.localizedDescription)"
                }
            }
        }
    }

    func refreshVoWiFi(force: Bool = false) {
        if voWiFiPollTask == nil { resumeVoWiFiCheckpoint() }
        guard case .running = backend.state, !actionInFlight, !fallbackRecoveryInFlight else { return }
        if !force && !voWiFiActionInFlight { return }
        guard !voWiFiRefreshInFlight else { return }
        let expectedGeneration = generation
        let expectedOperationRevision = voWiFiOperationRevision
        let expectedStatusRevision = voWiFiStatusRevision
        voWiFiRefreshInFlight = true
        Task {
            defer {
                if expectedGeneration == generation,
                   expectedOperationRevision == voWiFiOperationRevision,
                   expectedStatusRevision == voWiFiStatusRevision {
                    voWiFiRefreshInFlight = false
                }
            }
            do {
                let status: ReaderVoWiFiStatus = try await APIClient(timeoutInterval: 10)
                    .get("api/reader/vowifi")
                guard expectedGeneration == generation,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                publishDirectVoWiFiStatus(status)
            } catch {
                guard expectedGeneration == generation,
                      expectedOperationRevision == voWiFiOperationRevision,
                      expectedStatusRevision == voWiFiStatusRevision else { return }
                recordVoWiFiStatusReadFailure()
            }
        }
    }

    func enableVoWiFi() {
        guard !actionInFlight else { return }
        guard !voWiFiActionInFlight else { return }
        // A fresh start cannot borrow the previous owner's optional target.
        stopTargetCache = nil
        let expectedGeneration = generation
        voWiFiOperationRevision &+= 1
        invalidatePendingVoWiFiStatusReads()
        let operationRevision = voWiFiOperationRevision
        epdgError = nil
        commit { $0.voWiFiActionInFlight = true }
        let clientRequestID = UUID().uuidString
        let request = ReaderVoWiFiRequest(
                    enable: true,
                    proxy: nil,
                    proxyHost: proxyIP,
                    proxyPort: proxyPort,
                    epdg: customEPDG,
                    epdgPortText: customEPDGPort,
                    observeOperation: true,
                    clientRequestID: clientRequestID)
        do { pendingCheckpoint = try checkpoints.begin(id: clientRequestID, enable: true, request: request) }
        catch { markVoWiFiObservationPending("无法保存 VoWiFi 请求检查点；操作保持锁定"); return }
        Task {
            do {
                let accepted: VoWiFiAccepted = try await APIClient(timeoutInterval: 15)
                    .send("api/reader/vowifi", body: request)
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision else { return }
                guard let operation = accepted.operation,
                      operation.clientRequestID == clientRequestID else {
                    markVoWiFiObservationPending(
                        "启用已被接受，但结构化回执暂不可见；正在从 Rust owner 恢复")
                    await recoverVoWiFiOwner(
                        expectedGeneration: expectedGeneration,
                        operationRevision: operationRevision,
                        requestedEnable: true,
                        clientRequestID: clientRequestID)
                    return
                }
                cacheStopTarget(accepted.stopTarget, fromAcceptedStart: true)
                voWiFiOwnerOperation = operation
                pollVoWiFi(
                    expectedGeneration: expectedGeneration,
                    operationRevision: operationRevision,
                    operation: operation)
            } catch {
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision else { return }
                if mutationFailureRequiresStatusReconciliation(error) {
                    markVoWiFiObservationPending(
                        "启用请求已发出，但回执暂不可见；正在从 Rust owner 恢复")
                    await recoverVoWiFiOwner(
                        expectedGeneration: expectedGeneration,
                        operationRevision: operationRevision,
                        requestedEnable: true,
                        clientRequestID: clientRequestID)
                    return
                }
                markVoWiFiObservationPending("启用请求结果待确认：\(error.localizedDescription)")
                await recoverVoWiFiOwner(expectedGeneration: expectedGeneration,
                    operationRevision: operationRevision, requestedEnable: true, clientRequestID: clientRequestID)
            }
        }
    }

    func disableVoWiFi() {
        guard !actionInFlight else { return }
        // The checkpoint is saved before POST. Until its exact stop receipt
        // terminates, repeated clicks must observe that ID instead of creating
        // another stop while the previous start owner is still displayed.
        guard pendingCheckpoint?.requestedEnable != false else { return }
        if voWiFiActionInFlight {
            // A retained start owner can be cancelled. Repeating a stop while
            // its Rust owner is still unresolved must not create another owner.
            guard voWiFiOwnerOperation?.requestedEnable == true else { return }
        }
        activeStopTarget = stopTargetCache
        let originalStopTarget = activeStopTarget
        voWiFiPollTask?.cancel()
        voWiFiPollTask = nil
        voWiFiOperationRevision &+= 1
        invalidatePendingVoWiFiStatusReads()
        let operationRevision = voWiFiOperationRevision
        commit { state in
            state.voWiFiActionInFlight = true
            Self.clearVoWiFiSnapshotFields(&state)
        }
        let expectedGeneration = generation
        let ownerOperation = voWiFiOwnerOperation
        let clientRequestID = UUID().uuidString
        let request = ReaderVoWiFiRequest(enable: false, proxy: nil,
                                         observeOperation: true, operation: ownerOperation,
                                         clientRequestID: clientRequestID)
        do { pendingCheckpoint = try checkpoints.begin(id: clientRequestID, enable: false, request: request) }
        catch {
            checkpointFailure = true
            markVoWiFiObservationPending("无法保存 VoWiFi 请求检查点；操作保持锁定")
            Task { await fallbackStop(target: originalStopTarget, generation: expectedGeneration, revision: operationRevision) }
            return
        }
        Task {
            do {
                let accepted: VoWiFiAccepted = try await APIClient(timeoutInterval: 15).send(
                    "api/reader/vowifi", body: request)
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision else { return }
                guard let operation = accepted.operation,
                      operation.clientRequestID == clientRequestID else {
                    markVoWiFiObservationPending(
                        "关闭已被接受，但结构化回执暂不可见；正在从 Rust owner 恢复")
                    await recoverVoWiFiOwner(
                        expectedGeneration: expectedGeneration,
                        operationRevision: operationRevision,
                        requestedEnable: false,
                        clientRequestID: clientRequestID)
                    return
                }
                voWiFiOwnerOperation = operation
                pollVoWiFi(expectedGeneration: expectedGeneration,
                           operationRevision: operationRevision, operation: operation)
            } catch {
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision else { return }
                if fallbackAllowed(error) {
                    await fallbackStop(target: originalStopTarget, generation: expectedGeneration, revision: operationRevision)
                }
                if mutationFailureRequiresStatusReconciliation(error) {
                    markVoWiFiObservationPending(
                        "关闭请求已发出，但回执暂不可见；正在从 Rust owner 恢复")
                    await recoverVoWiFiOwner(
                        expectedGeneration: expectedGeneration,
                        operationRevision: operationRevision,
                        requestedEnable: false,
                        clientRequestID: clientRequestID)
                    return
                }
                markVoWiFiObservationPending("关闭请求结果待确认：\(error.localizedDescription)")
                await recoverVoWiFiOwner(expectedGeneration: expectedGeneration,
                    operationRevision: operationRevision, requestedEnable: false, clientRequestID: clientRequestID)
            }
        }
    }

    private func cacheStopTarget(_ target: VoWiFiStopTarget?, fromAcceptedStart: Bool = false) {
        guard activeStopTarget == nil, (!voWiFiActionInFlight || fromAcceptedStart), let target, target.valid(for: "reader") else { return }
        stopTargetCache = target
    }

    private func fallbackAllowed(_ error: Error) -> Bool {
        if let code = (error as? APIError)?.code,
           ["vowifi_request_ledger_full", "vowifi_request_ledger_unavailable"].contains(code) { return true }
        return mutationFailureRequiresStatusReconciliation(error)
    }

    private func fallbackStop(target: VoWiFiStopTarget?, generation expectedGeneration: Int, revision: UInt64) async {
        guard let target, target.valid(for: "reader"), target == activeStopTarget else { return }
        let query = target.query
        var first = true
        while !Task.isCancelled, generation == expectedGeneration, voWiFiOperationRevision == revision {
            do {
                let result: VoWiFiStopControlResponse
                if first { result = try await APIClient(timeoutInterval: 10).upload("api/reader/vowifi/stop-control?\(query)", data: Data()) }
                else { result = try await APIClient(timeoutInterval: 10).get("api/reader/vowifi/stop-control?\(query)") }
                guard generation == expectedGeneration, voWiFiOperationRevision == revision,
                      target == activeStopTarget, result.stopControl.matches(target) else { return }
                first = false
                temporaryStopMessage = result.stopControl.displayMessage
                markVoWiFiObservationPending("临时控制没有耐久回执或重启保证")
                if result.stopControl.finished || result.stopControl.pollAfterMS == 0 { return }
            } catch is CancellationError { return }
            catch {
                guard generation == expectedGeneration, voWiFiOperationRevision == revision else { return }
                temporaryStopMessage = "原会话临时停止结果未确认：\(error.localizedDescription)"
                markVoWiFiObservationPending("原请求检查点保持不变")
                // An unknown POST response is followed only by exact-target GET.
                // Repeated POST is unnecessary and must never recapture a target.
                first = false
                if let code = (error as? APIError)?.code,
                   ["vowifi_stop_target_stale", "invalid_vowifi_stop_target"].contains(code) { return }
            }
            do { try await Task.sleep(for: .milliseconds(1_500)) } catch { return }
        }
    }

    private func resumeVoWiFiCheckpoint() {
        guard voWiFiPollTask == nil else { return }
        do {
            pendingCheckpoint = try checkpoints.load()
            checkpointFailure = false
        }
        catch {
            checkpointFailure = true
            markVoWiFiObservationPending("VoWiFi 请求检查点不可读；操作保持锁定")
            return
        }
        guard let checkpoint = pendingCheckpoint else { return }
        voWiFiOperationRevision &+= 1
        let revision = voWiFiOperationRevision
        let expected = generation
        markVoWiFiObservationPending("正在恢复原 VoWiFi 请求回执")
        voWiFiPollTask = Task {
            await recoverVoWiFiOwner(expectedGeneration: expected, operationRevision: revision,
                requestedEnable: checkpoint.requestedEnable, clientRequestID: checkpoint.clientRequestID)
        }
    }

    private func recoverVoWiFiOwner(
        expectedGeneration: Int,
        operationRevision: UInt64,
        requestedEnable: Bool,
        clientRequestID: String
    ) async {
        while !Task.isCancelled,
              expectedGeneration == generation,
              operationRevision == voWiFiOperationRevision {
            do {
                let status: ReaderVoWiFiStatus = try await APIClient(timeoutInterval: 10)
                    .get("api/reader/vowifi?\(VoWiFiOperation.requestObservationQuery(clientRequestID: clientRequestID))")
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision else { return }
                if let owner = status.operation,
                   owner.clientRequestID == clientRequestID,
                   owner.requestedEnable == requestedEnable {
                    voWiFiOwnerOperation = owner
                    pollVoWiFi(
                        expectedGeneration: expectedGeneration,
                        operationRevision: operationRevision,
                        operation: owner)
                    return
                }
            } catch is CancellationError {
                return
            } catch {
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision else { return }
                if (error as? APIError)?.code == "vowifi_request_id_unknown" {
                    markVoWiFiObservationPending(
                        "原请求回执未找到；提交结果待安全核对，已保留请求检查点")
                    do {
                        // Reconcile only the exact frozen request. This endpoint
                        // records a receipt without dispatching the mutation.
                        guard let checkpoint = try checkpoints.load(),
                              checkpoint.clientRequestID == clientRequestID,
                              checkpoint.requestedEnable == requestedEnable else {
                            markVoWiFiObservationPending("原请求检查点不匹配；操作保持锁定，等待安全核对")
                            try await Task.sleep(for: .milliseconds(1_500))
                            continue
                        }
                        let reconciled: VoWiFiAccepted = try await APIClient(timeoutInterval: 10)
                            .upload("api/reader/vowifi/reconcile", data: checkpoint.payload)
                        guard expectedGeneration == generation,
                              operationRevision == voWiFiOperationRevision else { return }
                        guard let owner = reconciled.operation,
                              owner.clientRequestID == clientRequestID,
                              owner.requestedEnable == requestedEnable else {
                            markVoWiFiObservationPending("安全核对回执不匹配；操作保持锁定")
                            try await Task.sleep(for: .milliseconds(1_500))
                            continue
                        }
                        voWiFiOwnerOperation = owner
                        pollVoWiFi(expectedGeneration: expectedGeneration,
                                   operationRevision: operationRevision, operation: owner)
                        return
                    } catch is CancellationError {
                        return
                    } catch {
                        guard expectedGeneration == generation,
                              operationRevision == voWiFiOperationRevision else { return }
                        if (error as? APIError)?.code == "vowifi_reconcile_owner_unconfirmed" {
                            markVoWiFiObservationPending(
                                "原请求回执未找到；后台资源仍待安全核对，已保留请求检查点")
                        } else {
                            markVoWiFiObservationPending(
                                "原请求回执未找到；安全核对尚未完成：\(error.localizedDescription)；已保留请求检查点")
                        }
                    }
                } else {
                    markVoWiFiObservationPending(
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

    private func pollVoWiFi(expectedGeneration: Int, operationRevision: UInt64, operation: VoWiFiOperation) {
        voWiFiPollTask?.cancel()
        voWiFiPollTask = Task {
            var projection = operation
            while !Task.isCancelled {
                guard expectedGeneration == generation,
                      operationRevision == voWiFiOperationRevision,
                      voWiFiOwnerOperation?.matches(operation) == true else { return }
                if projection.terminal {
                    do {
                        try checkpoints.clear(terminal: projection)
                        pendingCheckpoint = try checkpoints.load()
                    checkpointFailure = false
                    } catch { markVoWiFiObservationPending("保存终结回执失败；操作保持锁定"); return }
                    if pendingCheckpoint != nil {
                        deferredTerminal = deferredTerminal ?? projection
                        voWiFiPollTask = nil
                        resumeVoWiFiCheckpoint()
                        return
                    }
                    let completion = deferredTerminal ?? projection
                    deferredTerminal = nil
                    finishVoWiFiObservation(completion.message, success: completion.succeeded)
                    return
                }
                if projection.observationExpired {
                    markVoWiFiObservationPending(projection.message)
                }
                do {
                    let delay = max(projection.pollAfterMS, projection.observationExpired ? 1_500 : 1)
                    try await Task.sleep(for: .milliseconds(delay))
                    try Task.checkCancellation()
                    let expectedStatusRevision = voWiFiStatusRevision
                    let status: ReaderVoWiFiStatus = try await APIClient(timeoutInterval: 10)
                        .get("api/reader/vowifi?\(operation.observationQuery)")
                    guard expectedGeneration == generation,
                          operationRevision == voWiFiOperationRevision,
                          voWiFiOwnerOperation?.matches(operation) == true else { return }
                    guard let observed = status.operation, observed.matches(operation),
                          observed.clientRequestID == operation.clientRequestID,
                          observed.requestedEnable == operation.requestedEnable else {
                        markVoWiFiObservationPending("操作代次不匹配或后端暂时缺少结构化状态；后台 owner 尚未确认结束")
                        projection = VoWiFiOperation.pendingObservation(from: projection)
                        continue
                    }
                    projection = observed
                    // Evaluate the new Rust receipt before publishing the
                    // companion status. Failed/expired/superseded owners may
                    // never flash a stale ready projection into the UI.
                    if projection.terminal || projection.observationExpired {
                        continue
                    }
                    // A status read invalidated by a card observation may not
                    // publish readiness; operation identity remains separately fenced.
                    if expectedStatusRevision == voWiFiStatusRevision {
                        _ = publishDirectVoWiFiStatus(status)
                    }
                } catch is CancellationError {
                    return
                } catch {
                    guard expectedGeneration == generation,
                          operationRevision == voWiFiOperationRevision else { return }
                    markVoWiFiObservationPending(
                        "操作观察中断：\(error.localizedDescription)；后台 owner 尚未确认结束")
                    projection = VoWiFiOperation.pendingObservation(from: projection)
                }
            }
        }
    }

    private func markVoWiFiObservationPending(_ message: String) {
        let message = temporaryStopMessage.map { "临时停止：\($0)；永久请求回执仍待核对。\(message)" } ?? message
        commit { state in
            state.voWiFiActionInFlight = true
            Self.clearVoWiFiSnapshotFields(&state)
            state.error = message
        }
    }

    private func finishVoWiFiObservation(_ message: String, success: Bool) {
        if pendingCheckpoint == nil && !checkpointFailure { activeStopTarget = nil; temporaryStopMessage = nil }

        voWiFiPollTask = nil
        commit { $0.voWiFiActionInFlight = false }
        toast(message, success, "Wi-Fi Calling")
        refreshVoWiFi(force: true)
    }

    func checkProxy() {
        proxyCheckTask?.cancel()
        proxyCheckRevision &+= 1
        let requestRevision = proxyCheckRevision
        proxyChecking = true
        proxyCheckTask = Task {
            defer {
                if requestRevision == proxyCheckRevision { proxyChecking = false }
            }
            do {
                let request = ProxyCheckRequest(
                    proxyHost: proxyIP,
                    proxyPort: proxyPort,
                    epdg: customEPDG,
                    sessionEPDG: voWiFiStatus?.epdgFqdn,
                    readerEPDG: epdgFQDN)
                let result: ProxyCheckResult = try await APIClient(timeoutInterval: 12)
                    .send("api/reader/vowifi/proxy-check", body: request)
                guard requestRevision == proxyCheckRevision else { return }
                proxyCheckResult = result
            } catch {
                guard requestRevision == proxyCheckRevision else { return }
                proxyCheckResult = ProxyCheckResult(
                    ok: false, probeMessage: nil, countryCode: nil, country: nil,
                    exitIP: nil, latencyMs: nil, udpReady: nil, transport: nil,
                    udpMessage: nil, effectiveEPDG: nil, epdgSource: nil,
                    error: "检测失败：\(error.localizedDescription)")
            }
        }
    }

    func saveEPDGConfig() {
        epdgError = nil
        toast(customEPDG.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? "已保存自动选择 ePDG 的原始意图"
            : "自定义 ePDG 原始配置已保存；启用时由 Rust 验证", true, "Wi-Fi Calling")
    }

    private func commit(_ update: (inout ReaderRuntimeState) -> Void) {
        var next = state
        update(&next)
        if pendingCheckpoint != nil || checkpointFailure { next.voWiFiActionInFlight = true }
        next.revision = state.revision &+ 1
        state = next
    }

    private func setActionInFlight(_ value: Bool) {
        commit { $0.actionInFlight = value }
    }

    private func setError(_ message: String?) {
        commit { $0.error = message }
    }

    private func invalidateVoWiFiOperationForCardTransition() {
        stopTargetCache = nil
        voWiFiPollTask?.cancel()
        voWiFiPollTask = nil
        voWiFiOwnerOperation = nil
        voWiFiOperationRevision &+= 1
        verifiedCardIdentity = CardIdentity(nil)
        cardInterruptionConfirmed = false
        invalidatePendingVoWiFiStatusReads()
        lastVoWiFiHealthRefreshAt = nil
        if pendingCheckpoint != nil { resumeVoWiFiCheckpoint() }
    }

    private func invalidatePendingVoWiFiStatusReads() {
        voWiFiStatusRevision &+= 1
        voWiFiRefreshInFlight = false
    }

    private func publishVoWiFiStatus(_ status: ReaderVoWiFiStatus) {
        cacheStopTarget(status.stopTarget)
        invalidatePendingVoWiFiStatusReads()
        consecutiveVoWiFiStatusReadFailures = 0
        commit { state in
            state.voWiFiStatus = status
            state.voWiFiLoaded = true
        }
    }

    @discardableResult
    private func publishDirectVoWiFiStatus(_ status: ReaderVoWiFiStatus) -> Bool {
        if status.tunnelState == "disconnected",
           (status.imsState == nil || status.imsState == "idle"),
           status.smsReady != true,
           status.callReady != true {
            // A keyless idle result may always withdraw readiness.
            publishVoWiFiStatus(status)
            return true
        }
        let currentReader = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let iccid = usimInfo?.iccid.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let imsi = usimInfo?.imsi.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let currentSIM = iccid.isEmpty ? imsi : iccid
        guard connected, !currentReader.isEmpty, !currentSIM.isEmpty,
              status.readerID?.trimmingCharacters(in: .whitespacesAndNewlines) == currentReader,
              status.simID?.trimmingCharacters(in: .whitespacesAndNewlines) == currentSIM,
              (!requiresReaderGeneration || (
                readerSessionGeneration != nil &&
                status.readerGeneration == readerSessionGeneration)) else {
            invalidatePendingVoWiFiStatusReads()
            commit { state in
                Self.clearVoWiFiSnapshotFields(&state)
                state.voWiFiLoaded = true
                state.error = "VoWiFi 状态与当前读卡器或 SIM 不匹配，已拒绝显示就绪"
            }
            return false
        }
        publishVoWiFiStatus(status)
        return true
    }

    private static func clearCardDerivedFields(_ state: inout ReaderRuntimeState) {
        clearCardIdentityFields(&state)
        clearVoWiFiFields(&state)
    }

    private static func clearCardIdentityFields(
        _ state: inout ReaderRuntimeState,
        preservingCapabilities: Bool = false
    ) {
        state.usimInfo = nil
        state.profileOperatorName = nil
        state.epdgFQDN = nil
        state.carrierConfigError = nil
        if !preservingCapabilities {
            state.hasESIM = false
            state.cardType = nil
        }
        state.error = nil
    }

    private static func clearVoWiFiFields(_ state: inout ReaderRuntimeState) {
        state.voWiFiStatus = nil
        state.voWiFiLoaded = false
        state.voWiFiActionInFlight = false
    }

    /// A direct status request has no trustworthy replacement value on
    /// failure. Two consecutive failures are enough to make the previous
    /// ready status stale, while one failure remains tolerant of a brief
    /// backend restart or transient local socket error.
    private func recordVoWiFiStatusReadFailure() {
        consecutiveVoWiFiStatusReadFailures &+= 1
        if consecutiveVoWiFiStatusReadFailures >= 2, !voWiFiActionInFlight {
            invalidatePendingVoWiFiStatusReads()
        }
        commit { state in
            state.voWiFiLoaded = true
            guard consecutiveVoWiFiStatusReadFailures >= 2,
                  !state.voWiFiActionInFlight else { return }
            state.voWiFiStatus = nil
        }
    }

    /// Clears only data that came from a failed aggregate snapshot. In-flight
    /// enable/disable operations own their own completion and must not be
    /// cancelled or visually reset by an unrelated reader snapshot failure.
    private static func clearVoWiFiSnapshotFields(_ state: inout ReaderRuntimeState) {
        state.voWiFiStatus = nil
        state.voWiFiLoaded = false
    }

    private static func clearDerivedFields(_ state: inout ReaderRuntimeState) {
        state.connected = false
        state.name = nil
        state.actionInFlight = false
        clearCardDerivedFields(&state)
    }

    private func toast(_ message: String, _ success: Bool, _ title: String?) {
        onToast?(message, success, title)
    }

    private struct EmptyResponse: Decodable {}
}
