import AVFoundation
import AudioToolbox
import CoreAudio
import Foundation
import Network

/// Authenticate a loopback datagram and pass only a complete 20 ms PCM frame
/// to playback. The caller may retire local ringback only when playback begins.
enum ReaderRemotePCMFrame {
    static func beginsPlayback(_ arrival: AudioPlaybackPolicy.Arrival) -> Bool {
        arrival == .start || arrival == .schedule
    }

    static func accepted(
        _ datagram: Data?, token: Data, frameSamples: Int,
        rejectMalformed: (Int) -> Void,
        play: (Data.SubSequence) -> Bool
    ) -> Bool {
        guard let datagram,
              datagram.count > token.count,
              datagram.starts(with: token) else { return false }
        let pcm = datagram.dropFirst(token.count)
        guard pcm.count == frameSamples * MemoryLayout<Int16>.size else {
            rejectMalformed(pcm.count)
            return false
        }
        return play(pcm)
    }
}

@_silgen_name("switch_audio_recovery_is_current")
private func switchAudioRecoveryIsCurrent(
    _ capturedSession: UInt64,
    _ currentSession: UInt64,
    _ capturedRecovery: UInt64,
    _ currentRecovery: UInt64,
    _ running: UInt8
) -> UInt8

@_silgen_name("switch_audio_microphone_conditioner_create")
private func switchAudioMicrophoneConditionerCreate(_ sampleRate: UInt32) -> UnsafeMutableRawPointer?

@_silgen_name("switch_audio_microphone_conditioner_reset_stream")
private func switchAudioMicrophoneConditionerResetStream(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_audio_microphone_conditioner_destroy")
private func switchAudioMicrophoneConditionerDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_audio_microphone_conditioner_process")
private func switchAudioMicrophoneConditionerProcess(
    _ handle: UnsafeMutableRawPointer,
    _ frame: UnsafeMutablePointer<Int16>?,
    _ frameLength: Int,
    _ systemVoiceProcessingActive: UInt8
) -> UInt8

/// Queue-confined platform seam. A supplied environment replaces only device
/// effects and the timer; session/ticket/tap/playback state stays in this bridge.
enum ReaderAudioRecoveryAction: String {
    case engineStart, engineStop, graphReconnect, tapInstall, tapRemove
    case playerStop, ringbackStop, detachPlayers, playerPlay
    case playbackAppend, playbackClear, playbackSchedule, converterReset
    case graphInstall, connectionStart, connectionCancel, handshake
}

struct ReaderAudioRecoveryEnvironment {
    let schedule: (TimeInterval, @escaping () -> Void) -> Void
    let record: (ReaderAudioRecoveryAction) -> Void
    let startEngine: () throws -> Void
    var beforeStart: () async -> Void = {}
    var afterInstall: () async -> Void = {}
    var microphoneAccess: () async -> Bool = { true }
}

/// The caller owns this identity before entering async startup. Cancellation is
/// recorded even if startup has not reached the audio queue yet.
final class ReaderAudioSessionTicket: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    fileprivate func cancel() {
        lock.lock()
        cancelled = true
        lock.unlock()
    }
}

/// Real-time PCM bridge for reader VoWiFi calls. Carrier RTP remains in the
/// backend SWu netstack; this loopback-only channel carries microphone/speaker
/// samples and cannot route IMS traffic around the configured proxy.
final class ReaderVoWiFiAudioBridge: @unchecked Sendable {
    static let shared = ReaderVoWiFiAudioBridge()

    private let queue = DispatchQueue(label: "com.proletarius.reader-audio", qos: .userInteractive)
    private let queueKey = DispatchSpecificKey<Void>()
    private lazy var engine = AVAudioEngine()
    private lazy var player = AVAudioPlayerNode()
    private lazy var ringbackPlayer = AVAudioPlayerNode()
    private let recoveryEnvironment: ReaderAudioRecoveryEnvironment?
    private var connection: NWConnection?
    private var sessionTicket: ReaderAudioSessionTicket?
    private var token = Data()
    private var microphoneTapGeneration: UInt64 = 0
    private var playbackFormat: AVAudioFormat?
    private var playbackInputFormat: AVAudioFormat?
    private var playbackConverter: AVAudioConverter?
    private var diagnosticURL: URL?
    private let diagnosticQueue = DispatchQueue(label: "com.switch.audio-diagnostics")
    private var receivedDatagrams = 0
    private var rejectedFrames = 0
    private var playedFrames = 0

    private func recordAudioDiagnostic(_ event: String) {
        guard recoveryEnvironment == nil, let url = diagnosticURL else { return }
        let line = "\(Date().timeIntervalSince1970) \(event) received=\(receivedDatagrams) accepted=\(playbackReceivedFrames) rejected=\(rejectedFrames) played=\(playedFrames) queued=\(playbackPolicy.queued) underruns=\(playbackPolicy.underruns) engine=\(engine.isRunning) player=\(player.isPlaying)\n"
        diagnosticQueue.async {
            try? BoundedBackendLog.append(Data(line.utf8), to: url, maximumBytes: 65_536, archives: 1)
        }
    }
    private var pcmSampleRate = 16_000
    private var pcmFrameSamples: Int { pcmSampleRate / 50 }
    private var running = false
    private var tapInstalled = false
    private var ringbackRunning = false
    private var pendingPlaybackBuffers: [AVAudioPCMBuffer] = []
    private let playbackPolicy = AudioPlaybackPolicy()
    private var playbackReceivedFrames = 0
    private var microphonePCM: [Int16] = []
    private var microphoneConditioner: UnsafeMutableRawPointer?
    private let microphoneQueueSlots = DispatchSemaphore(value: 8)
    private var microphoneRequested = false
    private var recoveringAudioRoute = false
    private var routeRecoveryAttempts = 0
    private var lastAudioRouteError: String?
    private var systemVoiceProcessingActive = false
    private var configurationObserver: NSObjectProtocol?
    private var recoveryTickets = AudioRouteRecoveryTicketLedger()

    private init(recoveryEnvironment: ReaderAudioRecoveryEnvironment? = nil) {
        self.recoveryEnvironment = recoveryEnvironment
        queue.setSpecific(key: queueKey, value: ())
        guard recoveryEnvironment == nil else { return }
        configurationObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: engine,
            queue: nil
        ) { [weak self] _ in
            self?.queue.async { self?.recoverFromAudioRouteChange() }
        }
    }

    func start(
        port: Int,
        token: String,
        captureMicrophone: Bool,
        localRingback: Bool,
        sampleRate: Int = 16_000,
        diagnosticURL: URL? = nil,
        session: ReaderAudioSessionTicket
    ) async -> String? {
        await withTaskCancellationHandler(operation: {
            await recoveryEnvironment?.beforeStart()
            // Retiring the preceding session, claiming its successor and
            // installing every graph/connection resource are one queue turn.
            // A hangup cannot be mistaken for the new startup generation.
            let installationError = syncOnAudioQueue { () -> String? in
                guard !session.isCancelled, !Task.isCancelled else { return Self.startCancelled }
                guard sampleRate == 16_000 else { return "不支持的通话采样率。" }
                guard port > 0, port <= Int(UInt16.max),
                      let nwPort = NWEndpoint.Port(rawValue: UInt16(port)) else {
                    return "读卡器音频桥端口无效。"
                }
                stopOnAudioQueue()
                sessionTicket = session
                self.diagnosticURL = diagnosticURL
                pcmSampleRate = sampleRate
                self.token = Data(token.utf8) + Data([0])
                // Keep the established 48 kHz stereo hardware graph and the
                // persistent converter for unmodified 16 kHz wideband PCM.
                guard let playbackFormat = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2),
                      let inputFormat = AVAudioFormat(standardFormatWithSampleRate: Double(sampleRate), channels: 1),
                      let converter = AVAudioConverter(from: inputFormat, to: playbackFormat) else {
                    stopOnAudioQueue()
                    return "无法创建通话播放格式。"
                }
                converter.primeMethod = .none
                self.playbackFormat = playbackFormat
                playbackInputFormat = inputFormat
                playbackConverter = converter
                platformAction(.graphInstall) {
                    engine.attach(player)
                    engine.attach(ringbackPlayer)
                    engine.connect(player, to: engine.mainMixerNode, format: playbackFormat)
                    engine.connect(ringbackPlayer, to: engine.mainMixerNode, format: playbackFormat)
                }
                platformAction(.connectionStart) {
                    let connection = NWConnection(host: "127.0.0.1", port: nwPort, using: .udp)
                    self.connection = connection
                    connection.stateUpdateHandler = { _ in }
                    connection.start(queue: queue)
                    receiveNext(connection)
                }
                return nil
            }
            if let installationError { return installationError }
            await recoveryEnvironment?.afterInstall()
            if captureMicrophone, let error = await enableMicrophone(session: session) {
                stop(session: session)
                return error
            }
            let startError = syncOnAudioQueue { () -> String? in
                guard isCurrent(session) else { return Self.startCancelled }
                if let error = startEngineOnQueue(generation: recoveryTickets.sessionGeneration, localRingback: localRingback) {
                    return error
                }
                // Cancellation can arrive while engine.start is in progress.
                guard isCurrent(session) else { return Self.startCancelled }
                platformAction(.handshake) {
                    connection?.send(content: self.token, completion: .contentProcessed { _ in })
                }
                return nil
            }
            if startError != nil { stop(session: session) }
            return startError
        }, onCancel: {
            self.stop(session: session)
        })
    }

    private static let startCancelled = "读卡器通话音频启动已取消。"

    private func isCurrent(_ session: ReaderAudioSessionTicket) -> Bool {
        sessionTicket === session && !session.isCancelled
    }

    private func startEngineOnQueue(generation: UInt64, localRingback: Bool) -> String? {
        guard recoveryTickets.sessionGeneration == generation else { return "读卡器通话音频启动已取消。" }
        do {
            reconnectPlaybackGraph()
            try startAudioEngine()
            running = true
            recordAudioDiagnostic("start rate=\(pcmSampleRate)")
            setLocalRingbackEnabledOnQueue(localRingback)
            return nil
        } catch {
            return "启动读卡器通话音频失败：\(error.localizedDescription)"
        }
    }

    private func platformAction(_ action: ReaderAudioRecoveryAction, _ operation: () -> Void) {
        recoveryEnvironment?.record(action)
        if recoveryEnvironment == nil { operation() }
    }

    private func startAudioEngine() throws {
        recoveryEnvironment?.record(.engineStart)
        if let recoveryEnvironment { try recoveryEnvironment.startEngine() }
        else { try engine.start() }
    }

    func enableMicrophone(session: ReaderAudioSessionTicket) async -> String? {
        await withTaskCancellationHandler(operation: {
            guard syncOnAudioQueue({ isCurrent(session) }) else { return Self.startCancelled }
            let granted = await requestMicrophoneAccess()
            return syncOnAudioQueue {
                // Permission completion belongs only to the captured ticket;
                // it cannot install a tap on a replacement call.
                guard isCurrent(session) else { return Self.startCancelled }
                guard granted else { return "请在系统设置中允许 Switch 使用麦克风。" }
                microphoneRequested = true
                if tapInstalled { return nil }
                let wasRunning = running
                if wasRunning {
                    resetPlaybackForEngineRestart()
                    platformAction(.engineStop) { engine.stop() }
                }
                let error = installMicrophoneTapOnQueue()
                if wasRunning {
                    do {
                        reconnectPlaybackGraph()
                        try startAudioEngine()
                        resumePlayersAfterEngineStart()
                    } catch {
                        return "切换麦克风后无法恢复通话音频：\(error.localizedDescription)"
                    }
                }
                return error
            }
        }, onCancel: {
            self.stop(session: session)
        })
    }

    private func installMicrophoneTapOnQueue() -> String? {
        let tapGeneration = microphoneTapGeneration &+ 1
        if let recoveryEnvironment {
            recoveryEnvironment.record(.tapInstall)
        } else if let error = installSystemMicrophoneTapOnQueue(tapGeneration: tapGeneration) {
            return error
        }
        microphoneTapGeneration = tapGeneration
        tapInstalled = true
        return nil
    }

    private func installSystemMicrophoneTapOnQueue(tapGeneration: UInt64) -> String? {
        let input = engine.inputNode
        // Best-effort system voice processing provides acoustic echo
        // cancellation and hardware-aware noise suppression. Some aggregate
        // audio devices do not support it, so failure must not block a call.
        do {
            if !input.isVoiceProcessingEnabled { try input.setVoiceProcessingEnabled(true) }
            systemVoiceProcessingActive = input.isVoiceProcessingEnabled
        } catch {
            // AirPods, aggregate devices and some external interfaces may not
            // expose the macOS voice-processing unit. The PCM conditioner below
            // remains active, so the call continues with a deterministic
            // software fallback instead of failing audio setup.
            try? input.setVoiceProcessingEnabled(false)
            systemVoiceProcessingActive = false
        }
        let inputFormat = input.outputFormat(forBus: 0)
        guard inputFormat.sampleRate > 0,
              inputFormat.channelCount > 0,
              let sendFormat = AVAudioFormat(
                commonFormat: .pcmFormatInt16,
                sampleRate: Double(pcmSampleRate),
                channels: 1,
                interleaved: true),
              let converter = AVAudioConverter(from: inputFormat, to: sendFormat) else {
            return "无法连接当前 Mac 麦克风或创建通话采样率转换器。"
        }
        if microphoneConditioner == nil {
            guard let conditioner = switchAudioMicrophoneConditionerCreate(UInt32(pcmSampleRate)) else {
                return "无法初始化 Rust 麦克风处理器。"
            }
            microphoneConditioner = conditioner
        }
        input.installTap(onBus: 0, bufferSize: 960, format: inputFormat) { [weak self] buffer, _ in
            self?.convertMicrophone(
                buffer, format: sendFormat, converter: converter,
                tapGeneration: tapGeneration)
        }
        return nil
    }

    func setLocalRingbackEnabled(_ enabled: Bool) {
        syncOnAudioQueue {
            setLocalRingbackEnabledOnQueue(enabled)
        }
    }

    private func setLocalRingbackEnabledOnQueue(_ enabled: Bool) {
        guard enabled else {
            if ringbackRunning {
                platformAction(.ringbackStop) { ringbackPlayer.stop() }
                ringbackRunning = false
            }
            return
        }
        guard !ringbackRunning,
              let format = playbackFormat,
              let buffer = makeRingbackBuffer(format: format) else { return }
        platformAction(.playerPlay) {
            ringbackPlayer.scheduleBuffer(buffer, at: nil, options: .loops)
            ringbackPlayer.play()
        }
        ringbackRunning = true
    }

    func audioRoute() -> (input: String, output: String) {
        (defaultDeviceName(selector: kAudioHardwarePropertyDefaultInputDevice),
         defaultDeviceName(selector: kAudioHardwarePropertyDefaultOutputDevice))
    }

    func audioRouteError() -> String? {
        syncOnAudioQueue { lastAudioRouteError }
    }

    func audioProcessingDescription() -> String {
        syncOnAudioQueue {
            systemVoiceProcessingActive
                ? "macOS 回声消除与降噪"
                : "软件抗干扰与限幅"
        }
    }

    func stop() {
        syncOnAudioQueue {
            stopOnAudioQueue()
        }
    }

    func stop(session: ReaderAudioSessionTicket) {
        session.cancel()
        syncOnAudioQueue {
            guard sessionTicket === session else { return }
            stopOnAudioQueue()
        }
    }

    private func stopOnAudioQueue() {
        recordAudioDiagnostic("stop")
        sessionTicket?.cancel()
        sessionTicket = nil
        recoveryTickets.retireSession()
        if tapInstalled {
            platformAction(.tapRemove) { engine.inputNode.removeTap(onBus: 0) }
            tapInstalled = false
        }
        microphoneTapGeneration &+= 1
        platformAction(.playerStop) { player.stop() }
        platformAction(.ringbackStop) { ringbackPlayer.stop() }
        ringbackRunning = false
        platformAction(.engineStop) { engine.stop() }
        platformAction(.detachPlayers) {
            if player.engine != nil { engine.detach(player) }
            if ringbackPlayer.engine != nil { engine.detach(ringbackPlayer) }
        }
        platformAction(.connectionCancel) { connection?.cancel() }
        connection = nil
        playbackFormat = nil
        playbackInputFormat = nil
        playbackConverter = nil
        receivedDatagrams = 0
        rejectedFrames = 0
        playedFrames = 0
        clearPendingPlayback()
        playbackPolicy.resetForSession()
        playbackReceivedFrames = 0
        microphonePCM.removeAll(keepingCapacity: true)
        if let microphoneConditioner {
            switchAudioMicrophoneConditionerDestroy(microphoneConditioner)
            self.microphoneConditioner = nil
        }
        microphoneRequested = false
        recoveringAudioRoute = false
        routeRecoveryAttempts = 0
        lastAudioRouteError = nil
        systemVoiceProcessingActive = false
        token = Data()
        running = false
    }

    private func requestMicrophoneAccess() async -> Bool {
        if let recoveryEnvironment { return await recoveryEnvironment.microphoneAccess() }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized:
            return true
        case .notDetermined:
            return await withCheckedContinuation { continuation in
                AVCaptureDevice.requestAccess(for: .audio) { granted in
                    continuation.resume(returning: granted)
                }
            }
        case .denied, .restricted:
            return false
        @unknown default:
            return false
        }
    }

    private func convertMicrophone(
        _ input: AVAudioPCMBuffer,
        format: AVAudioFormat,
        converter: AVAudioConverter,
        tapGeneration: UInt64
    ) {
        let ratio = format.sampleRate / input.format.sampleRate
        let capacity = max(AVAudioFrameCount(pcmFrameSamples), AVAudioFrameCount(Double(input.frameLength) * ratio) + 16)
        guard let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return }
        var supplied = false
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, state in
            if supplied {
                state.pointee = .noDataNow
                return nil
            }
            supplied = true
            state.pointee = .haveData
            return input
        }
        guard status != .error, output.frameLength > 0,
              let samples = output.int16ChannelData?[0] else { return }
        let count = Int(output.frameLength)
        // The input/output AVAudioPCMBuffer is owned by the tap callback and
        // may be reused immediately. Copy only the converted samples before
        // crossing to the serial route/playback queue.
        let copiedSamples = Array(UnsafeBufferPointer(start: samples, count: count))
        // A stalled output route must not queue seconds of stale microphone
        // audio or grow memory without bound. A dropped 20 ms frame is safer
        // than replaying it after the caller has resumed speaking.
        guard microphoneQueueSlots.wait(timeout: .now()) == .success else { return }
        let slots = microphoneQueueSlots
        queue.async { [weak self] in
            defer { slots.signal() }
            guard let self,
                  self.microphoneTapGeneration == tapGeneration,
                  self.running, self.microphoneRequested,
                  self.connection != nil else { return }
            self.enqueueMicrophoneSamples(copiedSamples)
        }
    }

    private func enqueueMicrophoneSamples(_ samples: [Int16]) {
        guard let connection else { return }
        microphonePCM.append(contentsOf: samples)
        var consumed = 0
        while microphonePCM.count - consumed >= pcmFrameSamples {
            var frame = Array(microphonePCM[consumed..<(consumed + pcmFrameSamples)])
            consumed += pcmFrameSamples
            guard conditionMicrophoneFrame(&frame) else { continue }
            let pcm = frame.withUnsafeBytes { Data($0) }
            connection.send(content: token + pcm, completion: .contentProcessed { _ in })
        }
        if consumed > 0 {
            microphonePCM.removeFirst(consumed)
        }
    }

    private func receiveNext(_ connection: NWConnection) {
        connection.receiveMessage { [weak self] data, _, _, error in
            guard let self, error == nil, self.connection === connection else { return }
            self.receivedDatagrams += 1
            if self.receivedDatagrams == 1 { self.recordAudioDiagnostic("first-datagram bytes=\(data?.count ?? 0)") }
            if ReaderRemotePCMFrame.accepted(
                data, token: self.token, frameSamples: self.pcmFrameSamples,
                rejectMalformed: self.rejectMalformedPCM,
                play: self.playPCM
            ) {
                self.setLocalRingbackEnabledOnQueue(false)
            }
            self.receiveNext(connection)
        }
    }

    private func rejectMalformedPCM(_ byteCount: Int) {
        rejectedFrames += 1
        if rejectedFrames == 1 {
            recordAudioDiagnostic("rejected bytes=\(byteCount) running=\(running)")
        }
    }

    private func playPCM(_ data: Data.SubSequence) -> Bool {
        let sourceFrames = data.count / 2
        guard running, sourceFrames == pcmFrameSamples, player.engine != nil,
              data.count == pcmFrameSamples * 2,
              let format = playbackInputFormat,
              let outputFormat = playbackFormat,
              let converter = playbackConverter,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(sourceFrames)),
              let samples = buffer.floatChannelData?[0] else {
            rejectMalformedPCM(data.count)
            return false
        }
        buffer.frameLength = AVAudioFrameCount(sourceFrames)
        let bytes = Array(data)
        for index in 0..<sourceFrames {
            let value = Int16(bitPattern: UInt16(bytes[index*2]) | UInt16(bytes[index*2+1]) << 8)
            samples[index] = Float(value) / 32_768
        }
        guard let output = AVAudioPCMBuffer(pcmFormat: outputFormat,
            frameCapacity: AVAudioFrameCount(sourceFrames * 3 + 64)) else { return false }
        var supplied = false
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            if supplied { inputStatus.pointee = .noDataNow; return nil }
            supplied = true
            inputStatus.pointee = .haveData
            return buffer
        }
        guard status != .error, conversionError == nil, output.frameLength > 0 else {
            rejectedFrames += 1
            recordAudioDiagnostic("conversion-failed status=\(status.rawValue)")
            return false
        }
        let beginsPlayback = enqueuePlaybackBuffer(output)
        playbackReceivedFrames += 1
        if playbackReceivedFrames == 1 || playbackReceivedFrames % 250 == 0 {
            recordAudioDiagnostic("pcm outputSamples=\(output.frameLength)")
        }
        return beginsPlayback
    }

    private func enqueuePlaybackBuffer(_ buffer: AVAudioPCMBuffer) -> Bool {
        let arrival = playbackPolicy.receive()
        switch arrival {
        case .hold:
            recoveryEnvironment?.record(.playbackAppend)
            pendingPlaybackBuffers.append(buffer)
        case .start:
            recoveryEnvironment?.record(.playbackAppend)
            pendingPlaybackBuffers.append(buffer)
            // Rust requests play once the 20 ms buffers reach its current
            // prime threshold. Swift only retains and schedules CoreAudio data.
            for item in pendingPlaybackBuffers { schedulePlaybackBuffer(item) }
            clearPendingPlayback()
            platformAction(.playerPlay) { player.play() }
        case .schedule:
            schedulePlaybackBuffer(buffer)
        case .drop:
            break
        }
        // Held frames remain in the prime queue. Ringback continues until
        // CoreAudio has actually been asked to play or schedule remote audio.
        return ReaderRemotePCMFrame.beginsPlayback(arrival)
    }

    private func schedulePlaybackBuffer(_ buffer: AVAudioPCMBuffer) {
        if let recoveryEnvironment {
            recoveryEnvironment.record(.playbackSchedule)
            return
        }
        let generation = playbackPolicy.generation
        player.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { [weak self] _ in
            guard let self else { return }
            self.queue.async {
                switch self.playbackPolicy.played(generation: generation, running: self.running) {
                case .ignore:
                    return
                case .counted:
                    self.playedFrames += 1
                case .pause:
                    self.playedFrames += 1
                    self.player.pause()
                }
            }
        }
    }

    private func conditionMicrophoneFrame(_ frame: inout [Int16]) -> Bool {
        guard !frame.isEmpty else { return true }
        guard let microphoneConditioner else {
            recordAudioDiagnostic("microphone-conditioner-missing")
            return false
        }
        let status = frame.withUnsafeMutableBufferPointer { samples in
            switchAudioMicrophoneConditionerProcess(
                microphoneConditioner,
                samples.baseAddress,
                samples.count,
                systemVoiceProcessingActive ? 1 : 0
            )
        }
        guard status == 1 else {
            recordAudioDiagnostic("microphone-conditioner-rejected-frame")
            return false
        }
        return true
    }

    private func makeRingbackBuffer(format: AVAudioFormat) -> AVAudioPCMBuffer? {
        // UK ringing cadence: 0.4s tone, 0.2s silence, 0.4s tone, 2s silence.
        let duration = 3.0
        let frames = AVAudioFrameCount(format.sampleRate * duration)
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames),
              let channels = buffer.floatChannelData else { return nil }
        buffer.frameLength = frames
        for frame in 0..<Int(frames) {
            let time = Double(frame) / format.sampleRate
            let toneOn = time < 0.4 || (time >= 0.6 && time < 1.0)
            let sample: Float = toneOn
                ? Float(0.10 * (sin(2 * .pi * 400 * time) + sin(2 * .pi * 450 * time)))
                : 0
            for channel in 0..<Int(format.channelCount) {
                channels[channel][frame] = sample
            }
        }
        return buffer
    }

    private func recoverFromAudioRouteChange() {
        guard running, !recoveringAudioRoute else { return }
        // A route-change notification can win the race with a delayed retry.
        // Retire every older ticket as soon as this recovery is claimed so a
        // stale timer cannot restart the engine again after this succeeds.
        recoveryTickets.claimRecovery()
        recoveringAudioRoute = true
        resetPlaybackForEngineRestart()
        platformAction(.engineStop) { engine.stop() }
        if tapInstalled {
            platformAction(.tapRemove) { engine.inputNode.removeTap(onBus: 0) }
            tapInstalled = false
        }
        microphoneTapGeneration &+= 1
        microphonePCM.removeAll(keepingCapacity: true)
        if let microphoneConditioner {
            switchAudioMicrophoneConditionerResetStream(microphoneConditioner)
        }
        let microphoneError = microphoneRequested ? installMicrophoneTapOnQueue() : nil
        do {
            reconnectPlaybackGraph()
            try startAudioEngine()
            resumePlayersAfterEngineStart()
            recoveringAudioRoute = false
            if microphoneError == nil {
                routeRecoveryAttempts = 0
                lastAudioRouteError = nil
            } else {
                lastAudioRouteError = microphoneError
                scheduleAudioRouteRecovery()
            }
        } catch {
            lastAudioRouteError = "音频设备切换后恢复失败：\(error.localizedDescription)"
            recoveringAudioRoute = false
            scheduleAudioRouteRecovery()
        }
    }

    /// Engine reconfiguration can discard scheduled buffers without delivering
    /// their playback callbacks. Invalidate those callbacks before stopping the
    /// nodes; otherwise stale counts permanently saturate the bounded queue.
    private func resetPlaybackForEngineRestart() {
        recordAudioDiagnostic("reset-playback-for-engine-restart")
        playbackPolicy.resetForRoute()
        platformAction(.playerStop) { player.stop() }
        platformAction(.ringbackStop) { ringbackPlayer.stop() }
        clearPendingPlayback()
        platformAction(.converterReset) { playbackConverter?.reset() }
    }

    private func clearPendingPlayback() {
        recoveryEnvironment?.record(.playbackClear)
        pendingPlaybackBuffers.removeAll(keepingCapacity: true)
    }

    private func reconnectPlaybackGraph() {
        platformAction(.graphReconnect) {
            guard let format = playbackFormat else { return }
            engine.disconnectNodeOutput(player)
            engine.disconnectNodeOutput(ringbackPlayer)
            engine.connect(player, to: engine.mainMixerNode, format: format)
            engine.connect(ringbackPlayer, to: engine.mainMixerNode, format: format)
        }
    }

    private func resumePlayersAfterEngineStart() {
        // Downlink primes again from fresh PCM, never from discarded schedules.
        if ringbackRunning {
            ringbackRunning = false
            setLocalRingbackEnabledOnQueue(true)
        }
        recordAudioDiagnostic("engine-restarted")
    }

    private func scheduleAudioRouteRecovery() {
        routeRecoveryAttempts += 1
        guard running else { return }
        guard routeRecoveryAttempts <= 5 else {
            if lastAudioRouteError == nil {
                lastAudioRouteError = "无法恢复当前音频输入输出设备，请检查系统声音设置。"
            }
            return
        }
        let ticket = recoveryTickets.scheduleRetry()
        let retry = { [weak self] in
            guard let self,
                  self.recoveryTickets.isCurrent(ticket, running: self.running),
                  switchAudioRecoveryIsCurrent(
                    ticket.sessionGeneration,
                    self.recoveryTickets.sessionGeneration,
                    ticket.recoveryGeneration,
                    self.recoveryTickets.recoveryGeneration,
                    self.running ? 1 : 0
                  ) == 1 else { return }
            self.recoverFromAudioRouteChange()
        }
        if let recoveryEnvironment {
            recoveryEnvironment.schedule(0.4) { [weak self] in self?.syncOnAudioQueue(retry) }
        } else {
            queue.asyncAfter(deadline: .now() + 0.4, execute: retry)
        }
    }

#if SWITCH_AUDIO_RECOVERY_TESTING
    // Startup enters the actual public async path. The remaining entry points
    // expose production recovery/playback transitions without alternate logic.
    static func recoveryTestBridge(_ environment: ReaderAudioRecoveryEnvironment) -> ReaderVoWiFiAudioBridge {
        ReaderVoWiFiAudioBridge(recoveryEnvironment: environment)
    }

    func recoveryTestStartSession() async -> String? {
        await start(port: 49999, token: "virtual-test", captureMicrophone: true,
                    localRingback: false, session: ReaderAudioSessionTicket())
    }

    func recoveryTestRouteChange() { syncOnAudioQueue { recoverFromAudioRouteChange() } }

    func recoveryTestEnqueue(_ buffer: AVAudioPCMBuffer) {
        syncOnAudioQueue { _ = enqueuePlaybackBuffer(buffer) }
    }

    struct RecoveryTestSnapshot: Equatable {
        let running: Bool
        let resourcesInstalled: Bool
        let tapInstalled: Bool
        let tapGeneration: UInt64
        let session: UInt64
        let recovery: UInt64
        let playbackGeneration: UInt64
        let queued: Int
        let pending: [ObjectIdentifier]
    }

    func recoveryTestSnapshot() -> RecoveryTestSnapshot {
        syncOnAudioQueue {
            RecoveryTestSnapshot(running: running, resourcesInstalled: playbackFormat != nil, tapInstalled: tapInstalled,
                tapGeneration: microphoneTapGeneration, session: recoveryTickets.sessionGeneration,
                recovery: recoveryTickets.recoveryGeneration, playbackGeneration: playbackPolicy.generation,
                queued: playbackPolicy.queued, pending: pendingPlaybackBuffers.map(ObjectIdentifier.init))
        }
    }
#endif

    private func syncOnAudioQueue<T>(_ work: () -> T) -> T {
        if DispatchQueue.getSpecific(key: queueKey) != nil {
            return work()
        } else {
            return queue.sync(execute: work)
        }
    }

    private func defaultDeviceName(selector: AudioObjectPropertySelector) -> String {
        var address = AudioObjectPropertyAddress(
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var deviceID: AudioDeviceID = 0
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &deviceID) == noErr,
              deviceID != 0 else { return "系统默认设备" }
        var nameAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceNameCFString,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var unmanagedName: Unmanaged<CFString>?
        size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(deviceID, &nameAddress, 0, nil, &size, &unmanagedName) == noErr,
              let unmanagedName else { return "系统默认设备" }
        return unmanagedName.takeRetainedValue() as String
    }
}
