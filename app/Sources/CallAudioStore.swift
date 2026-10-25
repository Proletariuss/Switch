import Combine
import Foundation

/// Owns host audio routing for module and reader VoWiFi calls.
/// Signalling state is supplied by `CallRuntimeStore`; an operation revision
/// fences in-flight work; a call identity fence also suppresses later stale
/// status updates while a reader hangup request is awaiting confirmation.
@MainActor
final class CallAudioStore: ObservableObject {
    @Published var error: String?
    @Published var running = false
    @Published var readerPhase = "未连接"
    @Published var readerInputName = "系统默认输入"
    @Published var readerOutputName = "系统默认输出"
    @Published var readerProcessingName = "软件抗干扰与限幅"
    @Published private(set) var readerMicrophoneActive = false

    var currentStatus: (() -> CallStatus)?
    var currentEndpoint: (() -> String)?
    var currentReaderState: (() -> ReaderVoWiFiCallState?)?
    var moduleCallModeReady: (() -> Bool)?

    private let backend: BackendProcess
    private var activationTask: Task<Void, Never>?
    private var routeChangeTask: Task<Void, Never>?
    private var operationRevision: UInt64 = 0
    private struct ReaderHangupFence {
        let callID: String?
        let token: String?

        init(_ call: ReaderVoWiFiCallState?) {
            callID = call?.callID.flatMap { $0.isEmpty ? nil : $0 }
            token = call?.audioToken.flatMap { $0.isEmpty ? nil : $0 }
        }

        func isDifferentCall(_ call: ReaderVoWiFiCallState) -> Bool {
            let next = ReaderHangupFence(call)
            if let callID, let nextID = next.callID { return callID != nextID }
            if let token, let nextToken = next.token { return token != nextToken }
            // Missing identity is not proof that the ended call was replaced.
            return false
        }
    }
    private var readerHangupFence: ReaderHangupFence?
    private var moduleHangupPending = false
    private struct ReaderAudioBinding {
        let callID: String?
        let port: Int
        let token: String

        func matches(_ call: ReaderVoWiFiCallState) -> Bool {
            call.callID == callID && call.audioPort == port && call.audioToken == token
        }
    }
    private var readerAudioBinding: ReaderAudioBinding?
    private var readerAudioSession: ReaderAudioSessionTicket?
    // Platform ownership is independent of signalling selection and UI flags.
    private enum AudioOwner { case moduleStarting, module, reader }
    private var audioOwner: AudioOwner?
    private var ownsModuleAudio: Bool { audioOwner == .moduleStarting || audioOwner == .module }
    // Potential backend ownership survives refused/failed starts until a stop
    // receipt settles the durable API generation. This is not a live HAL owner.
    private var moduleBackendRevision: UInt64?
    private var moduleBackendStart: Task<CallAudioStartResult, Error>?
    private var moduleBackendRetirement: Task<Void, Never>?
    private var moduleBackendResumeRevision: UInt64?

    init(backend: BackendProcess) {
        self.backend = backend
        AudioBridge.shared.onDefaultDeviceChange = { [weak self] in
            self?.scheduleModuleRouteRecovery()
        }
    }

    var activationInFlight: Bool { activationTask != nil }

    func reset() {
        stop(stopModuleRoute: false)
        readerHangupFence = nil
        moduleHangupPending = false
        if !AudioBridge.shared.lifecycleSnapshot.retirementPending,
           moduleBackendRevision == nil { error = nil }
        readerInputName = "系统默认输入"
        readerOutputName = "系统默认输出"
        readerProcessingName = "软件抗干扰与限幅"
    }

    func handleStatusChange(previous: CallStatus, status: CallStatus) {
        let endpoint = currentEndpoint?() ?? "module"
        let readerCall = currentReaderState?()
        let readerIntent = ReaderAudioAdmission.decide(status: status, call: readerCall)
        if isVoWiFiEndpoint(endpoint), ownsModuleAudio {
            stop(stopModuleRoute: true, resumeAfterRetirement: true)
        }
        var retiredReaderForEndpointChange = false
        if let binding = readerAudioBinding,
           !isVoWiFiEndpoint(endpoint) || readerCall.map(binding.matches) != true
            || !readerIntent.isAdmitted
            || (readerMicrophoneActive && !readerIntent.captureMicrophone) {
            // Polling can skip idle between calls. Even an incoming call with
            // no ready media, or fallback to an unknown module state, must
            // retire the preceding reader call's live microphone.
            retiredReaderForEndpointChange = !isVoWiFiEndpoint(endpoint)
            stop(stopModuleRoute: false)
        }
        if endpoint == "module", moduleHangupPending {
            if status.isIdle { moduleHangupPending = false }
            else { return }
        }
        if isVoWiFiEndpoint(endpoint), let fence = readerHangupFence {
            if status.isIdle || ["idle", "ended", "failed"].contains(readerCall?.state ?? "")
                || readerCall.map(fence.isDifferentCall) == true {
                readerHangupFence = nil
            } else {
                // Polls and an ACK may still report this call as active while
                // the hangup API is pending. They cannot authorize new audio.
                return
            }
        }
        if status.isActive {
            if endpoint == "module" {
                if !previous.isActive || retiredReaderForEndpointChange || audioOwner == nil { startModuleAudio() }
            } else if isVoWiFiEndpoint(endpoint),
                      readerIntent == .duplex,
                      let readerCall,
                      let port = readerCall.audioPort,
                      let token = readerCall.audioToken {
                refreshReaderRouteDescription()
                if audioOwner == .reader, running {
                    ReaderVoWiFiAudioBridge.shared.setLocalRingbackEnabled(false)
                    if !readerMicrophoneActive { enableReaderMicrophone() }
                } else {
                    startReaderAudio(
                        port: port,
                        token: token,
                        captureMicrophone: true,
                        localRingback: false)
                }
            }
        } else if isVoWiFiEndpoint(endpoint),
                  let readerCall,
                  readerIntent.isReceiveOnly,
                  let port = readerCall.audioPort,
                  let token = readerCall.audioToken {
            if audioOwner == .reader, running {
                ReaderVoWiFiAudioBridge.shared.setLocalRingbackEnabled(
                    readerIntent.localRingback)
                readerPhase = readerIntent.isIncomingReceiveOnly
                    ? "正在播放通话音频"
                    : (readerCall.earlyMedia ? "正在播放远端早期媒体" : "正在播放本地回铃音")
                refreshReaderRouteDescription()
            } else {
                startReaderAudio(
                    port: port,
                    token: token,
                    captureMicrophone: false,
                    localRingback: readerIntent.localRingback)
            }
        }
        if status.isIdle && !previous.isIdle { stop() }
    }

    private func matchesReaderAudio(port: Int, token: String) -> Bool {
        guard readerHangupFence == nil,
              isVoWiFiEndpoint(currentEndpoint?() ?? ""),
              let call = currentReaderState?(),
              let status = currentStatus?(),
              ReaderAudioAdmission.decide(status: status, call: call).isAdmitted,
              let binding = readerAudioBinding else { return false }
        return binding.port == port && binding.token == token && binding.matches(call)
    }

    func prepareForHangup(endpoint: String) {
        if isVoWiFiEndpoint(endpoint) {
            readerHangupFence = ReaderHangupFence(currentReaderState?())
        } else if endpoint == "module" {
            moduleHangupPending = true
        }
        stop(stopModuleRoute: false)
    }

    func startModuleAudio() {
        guard currentEndpoint?() == "module" else { return }
        startModuleAudio(activateBackendRoute: true)
    }

    private func startModuleAudio(activateBackendRoute: Bool) {
        guard moduleRetirementAllowsReplacement() else { return }
        guard !moduleHangupPending, activationTask == nil, !running else { return }
        if activateBackendRoute, moduleBackendRevision != nil {
            retireModuleBackendRoute(resume: true)
            return
        }
        guard moduleCallModeReady?() == true else {
            error = "通话已接通，但通话模式尚未就绪。请挂断后完成运行时准备。"
            return
        }
        operationRevision &+= 1
        let revision = operationRevision
        audioOwner = .moduleStarting
        error = nil
        activationTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if revision == self.operationRevision {
                    self.activationTask = nil
                    if self.audioOwner == .moduleStarting {
                        let snapshot = AudioBridge.shared.lifecycleSnapshot
                        self.audioOwner = snapshot.running || snapshot.retirementPending ? .module : nil
                    }
                }
            }
            do {
                try Task.checkCancellation()
                guard revision == self.operationRevision, !self.moduleHangupPending,
                      self.currentStatus?().isActive == true,
                      self.currentEndpoint?() == "module" else { return }
                guard await AudioBridge.shared.requestMicrophoneAccess() else {
                    guard revision == self.operationRevision else { return }
                    self.error = "麦克风权限未开启。请在系统设置“隐私与安全性 → 麦克风”中允许 Switch。"
                    return
                }
                // Permission can complete after hangup or endpoint replacement.
                // Fence the backend side effect, not just the local engine.
                try Task.checkCancellation()
                guard revision == self.operationRevision, !self.moduleHangupPending,
                      self.currentStatus?().isActive == true,
                      self.currentEndpoint?() == "module" else { return }
                guard self.moduleRetirementAllowsReplacement() else { return }
                if activateBackendRoute {
                    self.moduleBackendRevision = revision
                    let request = Task { () throws -> CallAudioStartResult in
                        try await APIClient(timeoutInterval: 60).send("api/call/audio/start")
                    }
                    self.moduleBackendStart = request
                    let result = try await request.value
                    guard revision == self.operationRevision else { return }
                    guard result.started else {
                        self.error = "模块没有确认通话音频路由已启动。"
                        self.retireModuleBackendRoute()
                        return
                    }
                }
                try Task.checkCancellation()
                guard revision == self.operationRevision,
                      self.currentStatus?().isActive == true,
                      self.currentEndpoint?() == "module" else {
                    if activateBackendRoute { self.retireModuleBackendRoute() }
                    return
                }
                if let bridgeError = AudioBridge.shared.start() {
                    self.error = bridgeError
                    self.running = AudioBridge.shared.lifecycleSnapshot.running
                    if activateBackendRoute { self.retireModuleBackendRoute() }
                    return
                }
                guard revision == self.operationRevision else {
                    if let moduleAudioStopError = AudioBridge.shared.stop() {
                        self.error = "本机模块音频清理未确认：\(moduleAudioStopError)"
                    }
                    return
                }
                self.running = true
                self.audioOwner = .module
            } catch is CancellationError {
                // The stop/hangup path owns route cleanup.
            } catch {
                guard revision == self.operationRevision else { return }
                self.error = "通话音频启动失败：\(error.localizedDescription)"
                self.running = AudioBridge.shared.lifecycleSnapshot.running
                self.retireModuleBackendRoute()
            }
        }
    }

    private func startReaderAudio(
        port: Int,
        token: String,
        captureMicrophone: Bool,
        localRingback: Bool
    ) {
        guard moduleRetirementAllowsReplacement(requireStopped: true) else { return }
        if moduleBackendRevision != nil {
            retireModuleBackendRoute(resume: true)
            return
        }
        guard readerHangupFence == nil, activationTask == nil, !running else { return }
        audioOwner = .reader
        readerAudioBinding = ReaderAudioBinding(callID: currentReaderState?()?.callID, port: port, token: token)
        let session = ReaderAudioSessionTicket()
        readerAudioSession = session
        operationRevision &+= 1
        let revision = operationRevision
        error = nil
        let startingIntent = ReaderAudioAdmission.decide(
            status: currentStatus?(), call: currentReaderState?())
        readerPhase = localRingback
            ? "正在建立回铃音输出"
            : (captureMicrophone ? "正在连接双向音频" : (startingIntent.isIncomingReceiveOnly ? "正在连接通话音频" : "正在连接早期媒体"))
        activationTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if revision == self.operationRevision {
                    self.activationTask = nil
                    // ACK may arrive while receive-only playback is attaching.
                    // Promote this same connection after attachment completes.
                    if self.running, !self.readerMicrophoneActive,
                       self.matchesReaderAudio(port: port, token: token),
                       ReaderAudioAdmission.decide(
                           status: self.currentStatus?(), call: self.currentReaderState?()) == .duplex {
                        ReaderVoWiFiAudioBridge.shared.setLocalRingbackEnabled(false)
                        self.enableReaderMicrophone()
                    }
                }
            }
            guard !Task.isCancelled, revision == self.operationRevision,
                  self.matchesReaderAudio(port: port, token: token) else { return }
            guard self.moduleRetirementAllowsReplacement(requireStopped: true) else { return }
            self.refreshReaderRouteDescription()
            let bridgeError = await ReaderVoWiFiAudioBridge.shared.start(
                port: port,
                token: token,
                captureMicrophone: captureMicrophone,
                localRingback: localRingback,
                sampleRate: 16_000,
                diagnosticURL: self.backend.logURL.deletingLastPathComponent().appendingPathComponent("audio-diagnostics.log"),
                session: session)
            guard !Task.isCancelled, revision == self.operationRevision else {
                ReaderVoWiFiAudioBridge.shared.stop(session: session)
                return
            }
            guard self.moduleRetirementAllowsReplacement(requireStopped: true) else { return }
            let completedIntent = ReaderAudioAdmission.decide(
                status: self.currentStatus?(), call: self.currentReaderState?())
            guard self.matchesReaderAudio(port: port, token: token),
                  completedIntent == .duplex
                    || (!captureMicrophone && completedIntent.isReceiveOnly) else {
                ReaderVoWiFiAudioBridge.shared.stop(session: session)
                return
            }
            self.error = bridgeError
            self.running = bridgeError == nil
            self.readerMicrophoneActive = bridgeError == nil && captureMicrophone
            self.readerProcessingName = ReaderVoWiFiAudioBridge.shared.audioProcessingDescription()
            if let bridgeError {
                self.readerPhase = bridgeError
            } else if localRingback {
                self.readerPhase = "正在播放本地回铃音"
            } else if captureMicrophone {
                self.readerPhase = "本机音频设备已启动"
            } else if completedIntent.isIncomingReceiveOnly {
                self.readerPhase = "正在播放通话音频"
            } else {
                self.readerPhase = "正在播放运营商早期媒体"
            }
        }
    }

    private func enableReaderMicrophone() {
        guard moduleRetirementAllowsReplacement(requireStopped: true) else { return }
        guard readerHangupFence == nil, activationTask == nil, running, !readerMicrophoneActive,
              ReaderAudioAdmission.decide(
                  status: currentStatus?(), call: currentReaderState?()) == .duplex,
              let call = currentReaderState?(),
              let port = call.audioPort, let token = call.audioToken,
              let session = readerAudioSession else { return }
        operationRevision &+= 1
        let revision = operationRevision
        readerPhase = "正在连接麦克风"
        activationTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if revision == self.operationRevision { self.activationTask = nil }
            }
            guard !Task.isCancelled, revision == self.operationRevision,
                  self.matchesReaderAudio(port: port, token: token),
                  ReaderAudioAdmission.decide(
                      status: self.currentStatus?(), call: self.currentReaderState?()) == .duplex else { return }
            guard self.moduleRetirementAllowsReplacement(requireStopped: true) else { return }
            let bridgeError = await ReaderVoWiFiAudioBridge.shared.enableMicrophone(session: session)
            guard !Task.isCancelled,
                  revision == self.operationRevision,
                  self.matchesReaderAudio(port: port, token: token),
                  ReaderAudioAdmission.decide(
                      status: self.currentStatus?(), call: self.currentReaderState?()) == .duplex else {
                ReaderVoWiFiAudioBridge.shared.stop(session: session)
                return
            }
            guard self.moduleRetirementAllowsReplacement(requireStopped: true) else { return }
            self.error = bridgeError
            self.readerMicrophoneActive = bridgeError == nil
            self.readerProcessingName = ReaderVoWiFiAudioBridge.shared.audioProcessingDescription()
            self.readerPhase = bridgeError ?? "麦克风已开启"
        }
    }

    func stop(stopModuleRoute: Bool = true, resumeAfterRetirement: Bool = false) {
        let shouldStopModuleRoute = stopModuleRoute && moduleBackendRevision != nil
        operationRevision &+= 1
        moduleBackendResumeRevision = nil
        activationTask?.cancel()
        activationTask = nil
        routeChangeTask?.cancel()
        routeChangeTask = nil
        let moduleAudioStopError = AudioBridge.shared.stop()
        if let session = readerAudioSession {
            ReaderVoWiFiAudioBridge.shared.stop(session: session)
        }
        readerAudioSession = nil
        readerAudioBinding = nil
        let snapshot = AudioBridge.shared.lifecycleSnapshot
        running = snapshot.running
        audioOwner = snapshot.retirementPending || snapshot.running ? .module : nil
        readerMicrophoneActive = false
        readerPhase = "未连接"
        if let moduleAudioStopError {
            error = "本机模块音频清理未确认：\(moduleAudioStopError)"
        }
        if shouldStopModuleRoute { retireModuleBackendRoute(resume: resumeAfterRetirement) }
    }

    /// No successor backend start is admitted until this captured generation's
    /// start has settled and its stop is acknowledged. APIClient supplies the
    /// durable audio_generation; endpoint changes cannot retarget this cleanup.
    private func retireModuleBackendRoute(resume: Bool = false) {
        if resume { moduleBackendResumeRevision = operationRevision }
        guard let owner = moduleBackendRevision else { return }
        guard case .running = backend.state else {
            error = "模块后端未运行，旧音频路由关闭未确认；恢复后请重试"
            return
        }
        guard moduleBackendRetirement == nil else { return }
        let start = moduleBackendStart
        moduleBackendRetirement = Task { [weak self] in
            _ = try? await start?.value
            guard let self, self.moduleBackendRevision == owner else { return }
            defer { self.moduleBackendRetirement = nil }
            do {
                let result: CallAudioStopResult = try await APIClient(timeoutInterval: 20)
                    .send("api/call/audio/stop")
                guard self.moduleBackendRevision == owner else { return }
                guard result.stopped else {
                    self.error = "模块音频路由关闭未确认，未启动替代音频"
                    return
                }
                self.moduleBackendRevision = nil
                self.moduleBackendStart = nil
                // A newer UI operation may have stopped or changed intent.
                // Re-evaluate current admission rather than captured endpoint.
                if self.moduleBackendResumeRevision == self.operationRevision,
                   let status = self.currentStatus?() {
                    self.moduleBackendResumeRevision = nil
                    self.handleStatusChange(previous: status, status: status)
                }
            } catch {
                guard self.moduleBackendRevision == owner else { return }
                self.error = "模块音频路由关闭未确认：\(error.localizedDescription)"
            }
        }
    }

    /// The platform owner's locked snapshot is authoritative even if a caller
    /// changed the published UI flag. No reader graph may replace unretired HAL
    /// registrations, including a stopped proc whose destroy is still pending.
    private func moduleRetirementAllowsReplacement(requireStopped: Bool = false) -> Bool {
        let snapshot = AudioBridge.shared.lifecycleSnapshot
        guard snapshot.retirementPending || (requireStopped && snapshot.running) else { return true }
        if let session = readerAudioSession {
            ReaderVoWiFiAudioBridge.shared.stop(session: session)
        }
        readerAudioSession = nil
        readerAudioBinding = nil
        readerMicrophoneActive = false
        readerPhase = "未连接"
        running = snapshot.running
        audioOwner = .module
        if error?.contains("未确认") != true {
            error = "本机模块音频清理未确认，请先重试停止旧路由"
        }
        return false
    }

    private func refreshReaderRouteDescription() {
        let route = ReaderVoWiFiAudioBridge.shared.audioRoute()
        readerInputName = route.input
        readerOutputName = route.output
        if let routeError = ReaderVoWiFiAudioBridge.shared.audioRouteError() {
            error = routeError
        } else if error?.hasPrefix("音频设备切换后") == true
                    || error?.hasPrefix("无法恢复当前音频") == true {
            error = nil
        }
    }

    private func scheduleModuleRouteRecovery() {
        routeChangeTask?.cancel()
        routeChangeTask = Task { [weak self] in
            do {
                try await Task.sleep(for: .milliseconds(350))
            } catch {
                return
            }
            guard let self, self.audioOwner != nil else { return }
            let endpoint = self.currentEndpoint?() ?? "module"
            if isVoWiFiEndpoint(endpoint) {
                if self.ownsModuleAudio {
                    self.stop(stopModuleRoute: true, resumeAfterRetirement: true)
                    if let status = self.currentStatus?() {
                        self.handleStatusChange(previous: status, status: status)
                    }
                    return
                }
                let call = self.currentReaderState?()
                let status = self.currentStatus?()
                let intent = ReaderAudioAdmission.decide(status: status, call: call)
                guard self.readerHangupFence == nil,
                      let call,
                      intent.mayRecoverRoute,
                      let port = call.audioPort,
                      let token = call.audioToken else { return }
                let captureMicrophone = intent.captureMicrophone
                self.operationRevision &+= 1
                self.activationTask?.cancel()
                self.activationTask = nil
                if let session = self.readerAudioSession {
                    ReaderVoWiFiAudioBridge.shared.stop(session: session)
                }
                self.running = false
                self.readerMicrophoneActive = false
                self.startReaderAudio(
                    port: port,
                    token: token,
                    captureMicrophone: captureMicrophone,
                    localRingback: false)
                return
            }
            guard endpoint == "module", self.audioOwner == .module,
                  self.currentStatus?().isActive == true else { return }
            self.operationRevision &+= 1
            self.activationTask?.cancel()
            self.activationTask = nil
            let stopError = AudioBridge.shared.stop()
            self.running = AudioBridge.shared.lifecycleSnapshot.running
            if let stopError {
                self.error = "音频设备切换后未确认旧路由退休：\(stopError)"
                return
            }
            self.startModuleAudio(activateBackendRoute: false)
        }
    }
}

private func isVoWiFiEndpoint(_ endpoint: String) -> Bool {
    endpoint == "reader" || endpoint == "module-vowifi"
}
