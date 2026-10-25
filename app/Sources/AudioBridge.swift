import Foundation
import CoreAudio
import AudioToolbox
import AVFoundation

@_silgen_name("switch_audio_module_devices_create")
private func moduleDevicesCreate() -> UnsafeMutableRawPointer?
@_silgen_name("switch_audio_module_devices_destroy")
private func moduleDevicesDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_audio_module_devices_add")
private func moduleDevicesAdd(
    _ handle: UnsafeMutableRawPointer, _ id: UInt32,
    _ name: UnsafePointer<UInt8>?, _ nameLength: Int,
    _ uid: UnsafePointer<UInt8>?, _ uidLength: Int,
    _ transport: UInt32, _ inputs: UInt32, _ outputs: UInt32,
    _ related: UnsafePointer<UInt32>?, _ relatedCount: Int, _ relatedKnown: UInt8
) -> UInt8
@_silgen_name("switch_audio_module_devices_select")
private func moduleDevicesSelect(_ handle: UnsafeMutableRawPointer, _ speaker: UInt32, _ microphone: UInt32) -> UInt64

/// Raw platform facts. Names, UID strings and transport values are not parsed
/// here; relatedDevices is CoreAudio's relationship property, nil if unreadable.
struct ModuleAudioDeviceObservation {
    let id: UInt32
    let name: String
    let uid: String?
    let transport: UInt32
    let inputStreams: UInt32
    let outputStreams: UInt32
    let relatedDevices: [UInt32]?
}

@_silgen_name("switch_audio_pcm_format_supported")
private func switchAudioPCMFormatSupported(_ bits: UInt32, _ isFloat: UInt8) -> UInt8

@_silgen_name("switch_audio_pcm_decode_mono")
private func switchAudioPCMDecodeMono(
    _ input: UnsafePointer<UInt8>?, _ inputLength: Int,
    _ bits: UInt32, _ isFloat: UInt8, _ channels: Int,
    _ output: UnsafeMutablePointer<Float32>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_audio_pcm_encode_mono")
private func switchAudioPCMEncodeMono(
    _ input: UnsafePointer<Float32>?, _ inputLength: Int,
    _ bits: UInt32, _ isFloat: UInt8, _ channels: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputLength: Int
) -> Int

@_silgen_name("switch_audio_pcm_front_trim_bytes")
private func switchAudioPCMFrontTrimBytes(
    _ queuedBytes: Int, _ maximumBytes: Int,
    _ bits: UInt32, _ isFloat: UInt8, _ channels: Int
) -> Int

@_silgen_name("switch_audio_route_is_safe")
private func switchAudioRouteIsSafe(
    _ defaultOutput: UInt32,
    _ defaultInput: UInt32,
    _ moduleInput: UInt32,
    _ moduleOutput: UInt32,
    _ outputSharesModuleUID: UInt8,
    _ inputSharesModuleUID: UInt8
) -> UInt8

@_silgen_name("switch_audio_resampler_create")
private func switchAudioResamplerCreate(_ inputRate: Double, _ outputRate: Double) -> UnsafeMutableRawPointer?

@_silgen_name("switch_audio_resampler_reset")
private func switchAudioResamplerReset(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_audio_resampler_destroy")
private func switchAudioResamplerDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_audio_resampler_process")
private func switchAudioResamplerProcess(
    _ handle: UnsafeMutableRawPointer,
    _ input: UnsafePointer<Float32>,
    _ inputLength: Int,
    _ output: UnsafeMutablePointer<Float32>,
    _ outputCapacity: Int
) -> Int

/// 语音通话音频桥：通话中实时路由音频（含重采样、声道与位深自适应）
///  下行：模块 AC Interface（8kHz 输入）→ 重采样 → Mac 内置扬声器
///  上行：Mac 内置麦克风 → 重采样 → 模块 AS Interface（8kHz 输出）
// CoreAudio 回调运行在实时线程；全部共享状态由 stateLock/bridgeLock 保护。
final class AudioBridge: @unchecked Sendable {
    static let shared = AudioBridge()

    private struct DeviceFormat {
        var sampleRate: Double = 0
        var channels: UInt32 = 0
        var bits: UInt32 = 16
        var isFloat = false
        var isNonInterleaved = false
    }

    private var moduleInDeviceID: AudioDeviceID = 0    // AC Interface（模块输入，下行）
    private var moduleOutDeviceID: AudioDeviceID = 0   // AS Interface（模块输出，上行）
    private var speakerDeviceID: AudioDeviceID = 0
    private var micDeviceID: AudioDeviceID = 0
    private var running = false
    private var retirementPending = false
    private let stateLock = NSLock()
    private struct IORegistration {
        let device: AudioDeviceID
        let procID: AudioDeviceIOProcID
    }
    private var registrations = AudioIOProcRetirementLedger<IORegistration>()

    // 音频管线状态（桥锁保护）
    private var downInputBytes = Data()
    private var upInputBytes = Data()
    private var downOutputSamples: [Float32] = []
    private var upOutputSamples: [Float32] = []
    private var downResampler: FloatResampler?
    private var upResampler: FloatResampler?
    private var moduleInFormat = DeviceFormat()
    private var moduleOutFormat = DeviceFormat()
    private var speakerFormat = DeviceFormat()
    private var micFormat = DeviceFormat()
    private let bridgeLock = NSLock()
    private var ioGeneration = AudioIOProcGenerationFence()
    private let deviceChangeQueue = DispatchQueue(label: "com.proletarius.module-audio-route")

    /// Delivered on the main queue when macOS changes its default input or
    /// output (for example when AirPods connect or change profile).
    var onDefaultDeviceChange: (() -> Void)?

    private init() {
#if !SWITCH_AUDIO_LIFETIME_TESTING
        for selector in [
            kAudioHardwarePropertyDefaultInputDevice,
            kAudioHardwarePropertyDefaultOutputDevice,
        ] {
            var address = AudioObjectPropertyAddress(
                mSelector: selector,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            AudioObjectAddPropertyListenerBlock(
                AudioObjectID(kAudioObjectSystemObject),
                &address,
                deviceChangeQueue
            ) { [weak self] _, _ in
                DispatchQueue.main.async { [weak self] in
                    self?.onDefaultDeviceChange?()
                }
            }
        }
#endif
    }

    var lifecycleSnapshot: AudioIOProcLifecycleSnapshot {
        stateLock.lock()
        defer { stateLock.unlock() }
        return AudioIOProcLifecycleSnapshot(running: running, retirementPending: retirementPending)
    }

    var isRunning: Bool { lifecycleSnapshot.running }

    func requestMicrophoneAccess() async -> Bool {
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

    // MARK: - 设备发现

    private var allDevices: [AudioDeviceID] {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size) == noErr else {
            return []
        }
        let count = Int(size) / MemoryLayout<AudioDeviceID>.size
        var devices = [AudioDeviceID](repeating: 0, count: count)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &devices) == noErr else {
            return []
        }
        return devices
    }

    private func deviceName(_ id: AudioDeviceID) -> String {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceNameCFString,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var unmanagedName: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(id, &address, 0, nil, &size, &unmanagedName) == noErr,
              let unmanagedName else {
            return ""
        }
        return unmanagedName.takeRetainedValue() as String
    }

    private func streamFormat(device: AudioDeviceID, scope: AudioObjectPropertyScope) -> DeviceFormat? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain)
        var format = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        guard AudioObjectGetPropertyData(device, &address, 0, nil, &size, &format) == noErr else {
            return nil
        }
        let isFloat = format.mFormatFlags & kLinearPCMFormatFlagIsFloat != 0
        let isSignedInteger = format.mFormatFlags & kLinearPCMFormatFlagIsSignedInteger != 0
        let isBigEndian = format.mFormatFlags & kLinearPCMFormatFlagIsBigEndian != 0
        let supportedWidth = switchAudioPCMFormatSupported(format.mBitsPerChannel, isFloat ? 1 : 0) == 1
        guard format.mFormatID == kAudioFormatLinearPCM,
              format.mSampleRate > 0,
              format.mChannelsPerFrame > 0,
              supportedWidth,
              (isFloat || isSignedInteger),
              !isBigEndian else {
            return nil
        }
        return DeviceFormat(
            sampleRate: format.mSampleRate,
            channels: format.mChannelsPerFrame,
            bits: format.mBitsPerChannel,
            isFloat: isFloat,
            isNonInterleaved: format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0)
    }

    func discoverDevices() -> (moduleIn: AudioDeviceID?, moduleOut: AudioDeviceID?, speaker: AudioDeviceID?, mic: AudioDeviceID?) {
        let speaker = defaultDevice(selector: kAudioHardwarePropertyDefaultOutputDevice)
        let mic = defaultDevice(selector: kAudioHardwarePropertyDefaultInputDevice)
        let observations = allDevices.map { device in
            ModuleAudioDeviceObservation(
                id: device, name: deviceName(device), uid: deviceUID(device),
                transport: deviceTransport(device),
                inputStreams: streamCount(device, scope: kAudioObjectPropertyScopeInput),
                outputStreams: streamCount(device, scope: kAudioObjectPropertyScopeOutput),
                relatedDevices: relatedDevices(device))
        }
        let pair = Self.selectModuleDevices(observations, speaker: speaker ?? 0, microphone: mic ?? 0)
        return (pair?.input, pair?.output, speaker, mic)
    }

    /// Production FFI adapter, also exercised by the no-device Swift harness.
    /// A new snapshot per call prevents stale enumeration facts from being reused.
    static func selectModuleDevices(
        _ observations: [ModuleAudioDeviceObservation], speaker: UInt32, microphone: UInt32
    ) -> (input: UInt32, output: UInt32)? {
        guard let handle = moduleDevicesCreate() else { return nil }
        defer { moduleDevicesDestroy(handle) }
        for observation in observations {
            let name = Array(observation.name.utf8)
            let uid = Array((observation.uid ?? "").utf8)
            let related = observation.relatedDevices ?? []
            let accepted = name.withUnsafeBufferPointer { nameBytes in
                uid.withUnsafeBufferPointer { uidBytes in
                    related.withUnsafeBufferPointer { relatedIDs in
                        moduleDevicesAdd(handle, observation.id,
                            nameBytes.baseAddress, nameBytes.count, uidBytes.baseAddress, uidBytes.count,
                            observation.transport, observation.inputStreams, observation.outputStreams,
                            relatedIDs.baseAddress, relatedIDs.count, observation.relatedDevices == nil ? 0 : 1)
                    }
                }
            }
            guard accepted == 1 else { return nil }
        }
        let packed = moduleDevicesSelect(handle, speaker, microphone)
        guard packed != 0 else { return nil }
        return (UInt32(packed >> 32), UInt32(truncatingIfNeeded: packed))
    }

    private func streamCount(_ id: AudioDeviceID, scope: AudioObjectPropertyScope) -> UInt32 {
        var address = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreams,
            mScope: scope, mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(id, &address, 0, nil, &size) == noErr,
              size % UInt32(MemoryLayout<AudioStreamID>.size) == 0 else { return 0 }
        return size / UInt32(MemoryLayout<AudioStreamID>.size)
    }

    private func deviceTransport(_ id: AudioDeviceID) -> UInt32 {
        var address = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyTransportType,
            mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var transport: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        guard AudioObjectGetPropertyData(id, &address, 0, nil, &size, &transport) == noErr,
              size == MemoryLayout<UInt32>.size else { return 0 }
        return transport
    }

    private func relatedDevices(_ id: AudioDeviceID) -> [AudioDeviceID]? {
        var address = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyRelatedDevices,
            mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(id, &address, 0, nil, &size) == noErr,
              size % UInt32(MemoryLayout<AudioDeviceID>.size) == 0,
              size <= 256 * MemoryLayout<AudioDeviceID>.size else { return nil }
        if size == 0 { return [] }
        var devices = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(id, &address, 0, nil, &size, &devices) == noErr,
              Int(size) == devices.count * MemoryLayout<AudioDeviceID>.size else { return nil }
        return devices
    }

    private func defaultDevice(selector: AudioObjectPropertySelector) -> AudioDeviceID? {
        var address = AudioObjectPropertyAddress(
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var deviceID: AudioDeviceID = 0
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &address,
            0,
            nil,
            &size,
            &deviceID) == noErr,
              deviceID != 0 else { return nil }
        return deviceID
    }

    private func deviceUID(_ id: AudioDeviceID) -> String? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var unmanagedUID: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(id, &address, 0, nil, &size, &unmanagedUID) == noErr,
              let unmanagedUID else {
            return nil
        }
        return unmanagedUID.takeRetainedValue() as String
    }

    // MARK: - 启动/停止

    func start() -> String? {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !running || retirementPending else { return nil }
        if let failure = retirePreviousIOProcs() { return failure }

        let (moduleIn, moduleOut, speaker, mic) = discoverDevices()
        guard let moduleIn, moduleIn != 0 else {
            return "未确认唯一模块的音频输入输出配对，请检查设备身份、连接及系统默认音频路由"
        }
        guard let moduleOut, moduleOut != 0 else {
            return "未找到模块音频输出设备（AS Interface）"
        }
        guard let speaker, speaker != 0 else {
            return "未找到系统默认音频输出设备"
        }
        guard let mic, mic != 0 else {
            return "未找到系统默认音频输入设备"
        }
        let moduleUIDs = Set([moduleIn, moduleOut].compactMap(deviceUID))
        let outputSharesModuleUID = deviceUID(speaker).map(moduleUIDs.contains) ?? false
        let inputSharesModuleUID = deviceUID(mic).map(moduleUIDs.contains) ?? false
        guard switchAudioRouteIsSafe(
            speaker,
            mic,
            moduleIn,
            moduleOut,
            outputSharesModuleUID ? 1 : 0,
            inputSharesModuleUID ? 1 : 0
        ) == 1 else {
            return "系统默认输入或输出与模块音频端点重合，请改为 Mac 内置设备、AirPods 或其他独立音频设备"
        }

        moduleInDeviceID = moduleIn
        moduleOutDeviceID = moduleOut
        speakerDeviceID = speaker
        micDeviceID = mic

        guard let detectedModuleInFormat = streamFormat(device: moduleIn, scope: kAudioObjectPropertyScopeInput),
              let detectedModuleOutFormat = streamFormat(device: moduleOut, scope: kAudioObjectPropertyScopeOutput),
              let detectedSpeakerFormat = streamFormat(device: speaker, scope: kAudioObjectPropertyScopeOutput),
              let detectedMicFormat = streamFormat(device: mic, scope: kAudioObjectPropertyScopeInput) else {
            return "音频设备不是受支持的原生端序 16/32 位整数或 32/64 位浮点 PCM 格式"
        }
        moduleInFormat = detectedModuleInFormat
        moduleOutFormat = detectedModuleOutFormat
        speakerFormat = detectedSpeakerFormat
        micFormat = detectedMicFormat

        guard let downResampler = FloatResampler(
            inRate: moduleInFormat.sampleRate,
            outRate: speakerFormat.sampleRate
        ), let upResampler = FloatResampler(
            inRate: micFormat.sampleRate,
            outRate: moduleOutFormat.sampleRate
        ) else {
            return "音频采样率无法由 Rust 音频核心安全处理"
        }
        self.downResampler = downResampler
        self.upResampler = upResampler

        return startIOProcs()
    }

    private func startIOProcs() -> String? {
        guard let generation = beginIOGeneration() else {
            resetAudioPipeline()
            return "音频回调代际已耗尽或平台位宽不受支持，请重新启动应用"
        }
        if let registrationStatus = registerIOProcs(generation: generation) {
            let retirementFailure = unregisterIOProcs()
            reconcileRetirementState()
            if let retirementFailure {
                return "音频设备注册失败（OSStatus \(registrationStatus)）；\(retirementMessage(retirementFailure))"
            }
            return "音频设备注册失败（OSStatus \(registrationStatus)）"
        }
        running = true
        retirementPending = false
        return nil
    }

    func stop() -> String? {
        stateLock.lock()
        defer { stateLock.unlock() }
        let failure = unregisterIOProcs()
        reconcileRetirementState()
        return failure.map(retirementMessage)
    }

    private func retirePreviousIOProcs() -> String? {
        guard !registrations.isEmpty else { return nil }
        let failure = unregisterIOProcs()
        reconcileRetirementState()
        guard registrations.isEmpty else {
            return failure.map(retirementMessage) ?? "上一次音频设备清理尚未确认"
        }
        return nil
    }

    private func reconcileRetirementState() {
        // A stop error cannot be projected as stopped. Retained registrations
        // also block the normal running-route idempotent-start shortcut.
        running = registrations.callbacksMayRun
        retirementPending = !registrations.isEmpty
        if !registrations.callbacksMayRun { resetAudioPipeline() }
    }

    private func resetAudioPipeline() {
        bridgeLock.lock()
        // A callback already inside this lock finishes first. A delayed entry
        // has only a scalar cookie and is rejected before touching reset data.
        ioGeneration.deactivate()
        downInputBytes.removeAll()
        upInputBytes.removeAll()
        downOutputSamples.removeAll()
        upOutputSamples.removeAll()
        downResampler = nil
        upResampler = nil
        bridgeLock.unlock()
    }

    // MARK: - IOProc

    private func beginIOGeneration() -> UInt? {
        bridgeLock.lock()
        defer { bridgeLock.unlock() }
        return ioGeneration.begin()
    }

    private static func dispatchIOProc(
        cookie: UnsafeMutableRawPointer?,
        input: UnsafePointer<AudioBufferList>?, output: UnsafeMutablePointer<AudioBufferList>?
    ) -> OSStatus {
        guard let cookie = AudioIOProcCookie(opaque: cookie) else { return noErr }
        return shared.ioProc(cookie: cookie, input: input, output: output)
    }

    private func registerIOProcs(generation: UInt) -> OSStatus? {
        let routes: [(AudioDeviceID, Int)] = [
            (moduleInDeviceID, 0),    // 模块输入（下行数据源）
            (moduleOutDeviceID, 1),   // 模块输出（上行目标）
            (speakerDeviceID, 2),     // 扬声器（下行播放目标）
            (micDeviceID, 3),         // 麦克风（上行数据源）
        ]
        for (device, role) in routes {
            var procID: AudioDeviceIOProcID?
            guard let cookie = AudioIOProcCookie(generation: generation, role: role) else {
                return kAudioHardwareUnspecifiedError
            }
            let err = createIOProc(device: device, cookie: cookie.opaque, procID: &procID)
            if err != noErr {
                return err
            }
            guard let procID else {
                return kAudioHardwareUnspecifiedError
            }
            let registrationIndex = registrations.append(
                IORegistration(device: device, procID: procID))
            let startStatus = startIOProc(device: device, procID: procID)
            guard startStatus == noErr else {
                // A failed AudioDeviceStart did not acknowledge active I/O;
                // The created IOProc remains in the retirement ledger until
                // CoreAudio confirms destruction; its cookie owns no memory.
                return startStatus
            }
            registrations.markStarted(at: registrationIndex)
        }
        return nil
    }

    private func unregisterIOProcs() -> AudioIOProcRetirementFailure? {
        registrations.retire(
            stop: { self.stopIOProc(device: $0.device, procID: $0.procID) },
            destroy: { self.destroyIOProc(device: $0.device, procID: $0.procID) },
            release: { _ in })
    }

    private func createIOProc(device: AudioDeviceID, cookie: UnsafeMutableRawPointer, procID: inout AudioDeviceIOProcID?) -> OSStatus {
#if SWITCH_AUDIO_LIFETIME_TESTING
        let result = lifetimePlatform.create(device, cookie)
        procID = result.1
        return result.0
#else
        return AudioDeviceCreateIOProcID(device, { _, _, input, _, output, _, clientData -> OSStatus in
            AudioBridge.dispatchIOProc(cookie: clientData, input: input, output: output)
        }, cookie, &procID)
#endif
    }

    private func startIOProc(device: AudioDeviceID, procID: AudioDeviceIOProcID) -> OSStatus {
#if SWITCH_AUDIO_LIFETIME_TESTING
        return lifetimePlatform.start(device, procID)
#else
        return AudioDeviceStart(device, procID)
#endif
    }

    private func stopIOProc(device: AudioDeviceID, procID: AudioDeviceIOProcID) -> OSStatus {
#if SWITCH_AUDIO_LIFETIME_TESTING
        return lifetimePlatform.stop(device, procID)
#else
        return AudioDeviceStop(device, procID)
#endif
    }

    private func destroyIOProc(device: AudioDeviceID, procID: AudioDeviceIOProcID) -> OSStatus {
#if SWITCH_AUDIO_LIFETIME_TESTING
        return lifetimePlatform.destroy(device, procID)
#else
        return AudioDeviceDestroyIOProcID(device, procID)
#endif
    }

    private func retirementMessage(_ failure: AudioIOProcRetirementFailure) -> String {
        "\(failure.operation.rawValue) 失败（OSStatus \(failure.status)）；旧音频注册仍由原路由持有，未启动替代音频路由"
    }

    private func ioProc(cookie: AudioIOProcCookie, input: UnsafePointer<AudioBufferList>?, output: UnsafeMutablePointer<AudioBufferList>?) -> OSStatus {
        bridgeLock.lock()
        defer { bridgeLock.unlock() }
        guard ioGeneration.admits(cookie) else { return noErr }
#if SWITCH_AUDIO_LIFETIME_TESTING
        lifetimePipelineAccesses += 1
        lifetimeInsidePipeline?()
#endif

        switch cookie.role {
        case 0: // 模块输入：读下行（对方语音）
            if let input {
                appendBufferList(input, format: moduleInFormat, to: &downInputBytes)
            }
        case 1: // 模块输出：写上行（发给网络）
            if let output {
                let samples = takeSamples(from: &upInputBytes, format: micFormat)
                let resampled = upResampler?.process(samples) ?? samples
                upOutputSamples.append(contentsOf: resampled)
                capSamples(&upOutputSamples, sampleRate: moduleOutFormat.sampleRate)
                writeSamples(&upOutputSamples, format: moduleOutFormat, into: output)
            }
        case 2: // 扬声器：写下行（播放对方语音）
            if let output {
                let samples = takeSamples(from: &downInputBytes, format: moduleInFormat)
                let resampled = downResampler?.process(samples) ?? samples
                downOutputSamples.append(contentsOf: resampled)
                capSamples(&downOutputSamples, sampleRate: speakerFormat.sampleRate)
                writeSamples(&downOutputSamples, format: speakerFormat, into: output)
            }
        case 3: // 麦克风：读上行（我说话）
            if let input {
                appendBufferList(input, format: micFormat, to: &upInputBytes)
            }
        default:
            break
        }
        return noErr
    }

    // MARK: - 音频数据处理

    private func appendBufferList(_ buffers: UnsafePointer<AudioBufferList>, format: DeviceFormat, to data: inout Data) {
        let pointer = UnsafeMutablePointer<AudioBufferList>(mutating: buffers)
        let list = UnsafeMutableAudioBufferListPointer(pointer)
        // 非交错多声道输入的每个 AudioBuffer 是一个独立声道；通话桥需要单声道，
        // 因此只取第一个缓冲。单缓冲的交错格式仍按 ASBD channels 正常降混。
        let selected = list.count > 1 ? list.prefix(1) : list[...]
        for buffer in selected {
            if let base = buffer.mData {
                data.append(base.assumingMemoryBound(to: UInt8.self), count: Int(buffer.mDataByteSize))
            }
        }
        let maximumBufferedBytes = 1_048_576
        if data.count > maximumBufferedBytes {
            // Drop whole PCM frames only. Otherwise an interleaved input can
            // resume at a different channel after queue overload. This Rust
            // policy helper runs only on overflow, not every audio callback.
            let channels = format.isNonInterleaved ? 1 : max(1, Int(format.channels))
            let trimBytes = switchAudioPCMFrontTrimBytes(
                data.count, maximumBufferedBytes,
                format.bits, format.isFloat ? 1 : 0, channels)
            if trimBytes >= 0 && trimBytes <= data.count {
                data.removeFirst(trimBytes)
            } else {
                // An invalid format cannot be trimmed without risking frame
                // misalignment or unbounded growth. start() validates formats.
                data.removeAll()
            }
        }
    }

    /// Keep CoreAudio's byte queue here; Rust selects channel 0 and decodes PCM.
    private func takeSamples(from data: inout Data, format: DeviceFormat) -> [Float32] {
        // CoreAudio exposes non-interleaved channels as separate AudioBuffers;
        // appendBufferList intentionally keeps the first channel, so its byte
        // queue contains one sample per frame rather than all ASBD channels.
        let channels = format.isNonInterleaved ? 1 : max(1, Int(format.channels))
        let bytesPerSample = Int(format.bits) / 8
        let frameBytes = channels * bytesPerSample
        guard !data.isEmpty, bytesPerSample > 0, frameBytes > 0 else { return [] }
        let usable = (data.count / frameBytes) * frameBytes
        guard usable > 0 else { return [] }

        var samples = [Float32](repeating: 0, count: usable / frameBytes)
        let decoded = data.withUnsafeBytes { raw in
            samples.withUnsafeMutableBufferPointer { output in
                switchAudioPCMDecodeMono(
                    raw.bindMemory(to: UInt8.self).baseAddress, usable,
                    format.bits, format.isFloat ? 1 : 0, channels,
                    output.baseAddress, output.count)
            }
        }
        guard decoded == samples.count else { return [] }
        data.removeFirst(usable)
        return samples
    }

    /// 将单声道 Float32 队列写入输出缓冲。每个 AudioBuffer 自己声明声道数，
    /// 因而同时兼容单缓冲交错与多缓冲非交错 CoreAudio 设备。
    private func writeSamples(_ samples: inout [Float32], format: DeviceFormat, into buffers: UnsafeMutablePointer<AudioBufferList>) {
        let list = UnsafeMutableAudioBufferListPointer(buffers)
        let bytesPerSample = Int(format.bits) / 8
        guard bytesPerSample > 0, !list.isEmpty else { return }
        var frameCapacity = Int.max
        for buffer in list {
            let channels = max(1, Int(buffer.mNumberChannels))
            frameCapacity = min(frameCapacity, Int(buffer.mDataByteSize) / (bytesPerSample * channels))
        }
        guard frameCapacity != Int.max else { return }
        let framesToWrite = min(samples.count, frameCapacity)

        for buffer in list {
            guard let base = buffer.mData else { continue }
            let channels = max(1, Int(buffer.mNumberChannels))
            let outputBytes = frameCapacity * channels * bytesPerSample
            let encoded = samples.withUnsafeBufferPointer { input in
                switchAudioPCMEncodeMono(
                    input.baseAddress, framesToWrite,
                    format.bits, format.isFloat ? 1 : 0, channels,
                    base.assumingMemoryBound(to: UInt8.self), outputBytes)
            }
            guard encoded == framesToWrite else {
                memset(base, 0, Int(buffer.mDataByteSize))
                return
            }
        }
        if framesToWrite > 0 {
            samples.removeFirst(framesToWrite)
        }
    }

    private func capSamples(_ samples: inout [Float32], sampleRate: Double) {
        let maximum = max(1, Int(sampleRate * 0.5))
        if samples.count > maximum {
            samples.removeFirst(samples.count - maximum)
        }
    }

#if SWITCH_AUDIO_LIFETIME_TESTING
    // Test builds replace only HAL registration calls/listeners. These hooks
    // exercise the production generation, registration, retirement and callback
    // adapter without device enumeration, microphone access or real audio I/O.
    struct LifetimeTestPlatform {
        var create: (AudioDeviceID, UnsafeMutableRawPointer) -> (OSStatus, AudioDeviceIOProcID?)
        var start: (AudioDeviceID, AudioDeviceIOProcID) -> OSStatus
        var stop: (AudioDeviceID, AudioDeviceIOProcID) -> OSStatus
        var destroy: (AudioDeviceID, AudioDeviceIOProcID) -> OSStatus
    }
    private var lifetimePlatform = LifetimeTestPlatform(
        create: { _, _ in preconditionFailure("install isolated HAL test platform") },
        start: { _, _ in preconditionFailure("install isolated HAL test platform") },
        stop: { _, _ in preconditionFailure("install isolated HAL test platform") },
        destroy: { _, _ in preconditionFailure("install isolated HAL test platform") })
    private var lifetimePipelineAccesses = 0
    private var lifetimeInsidePipeline: (() -> Void)?

    func lifetimeTestInstall(_ platform: LifetimeTestPlatform) {
        stateLock.lock()
        defer { stateLock.unlock() }
        precondition(registrations.isEmpty)
        lifetimePlatform = platform
        moduleInDeviceID = 1
        moduleOutDeviceID = 2
        speakerDeviceID = 3
        micDeviceID = 4
    }

    func lifetimeTestStartIO() -> String? {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !running || retirementPending else { return nil }
        // Same replacement-route admission as start(), without device queries.
        if let failure = retirePreviousIOProcs() { return failure }
        bridgeLock.lock()
        downInputBytes = Data([0x5a])
        bridgeLock.unlock()
        return startIOProcs()
    }

    func lifetimeTestSetInsidePipeline(_ hook: (() -> Void)?) {
        bridgeLock.lock()
        lifetimeInsidePipeline = hook
        bridgeLock.unlock()
    }

    func lifetimeTestSnapshot() -> (generation: UInt?, last: UInt, accesses: Int, bytes: Int, registrations: Int, retirementPending: Bool) {
        stateLock.lock()
        defer { stateLock.unlock() }
        bridgeLock.lock()
        defer { bridgeLock.unlock() }
        return (ioGeneration.activeGeneration, ioGeneration.lastGeneration,
                lifetimePipelineAccesses, downInputBytes.count, registrations.entries.count, retirementPending)
    }

    static func lifetimeTestInvoke(_ cookie: UnsafeMutableRawPointer?) -> OSStatus {
        dispatchIOProc(cookie: cookie, input: nil, output: nil)
    }
#endif
}

/// 线性插值重采样器（Float32 单声道，跨帧保持状态）
final class FloatResampler {
    private let inRate: Double
    private let outRate: Double
    private let handle: UnsafeMutableRawPointer

    init?(inRate: Double, outRate: Double) {
        guard let handle = switchAudioResamplerCreate(inRate, outRate) else { return nil }
        self.inRate = inRate
        self.outRate = outRate
        self.handle = handle
    }

    deinit {
        switchAudioResamplerDestroy(handle)
    }

    func reset() {
        switchAudioResamplerReset(handle)
    }

    func process(_ input: [Float32]) -> [Float32] {
        guard !input.isEmpty else { return [] }
        let estimate = Int(ceil(Double(input.count) * outRate / inRate)) + 4
        var output = [Float32](repeating: 0, count: max(4, estimate))
        let count = input.withUnsafeBufferPointer { source in
            output.withUnsafeMutableBufferPointer { destination in
                switchAudioResamplerProcess(
                    handle,
                    source.baseAddress!,
                    source.count,
                    destination.baseAddress!,
                    destination.count)
            }
        }
        guard count >= 0, count <= output.count else { return [] }
        output.removeSubrange(count..<output.count)
        return output
    }
}
