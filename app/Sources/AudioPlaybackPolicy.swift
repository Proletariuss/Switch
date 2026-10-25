import Foundation

@_silgen_name("switch_audio_playback_policy_create")
private func playbackPolicyCreate() -> UnsafeMutableRawPointer?
@_silgen_name("switch_audio_playback_policy_destroy")
private func playbackPolicyDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_audio_playback_policy_receive")
private func playbackPolicyReceive(_ handle: UnsafeMutableRawPointer) -> UInt8
@_silgen_name("switch_audio_playback_policy_played")
private func playbackPolicyPlayed(_ handle: UnsafeMutableRawPointer, _ generation: UInt64, _ running: UInt8) -> UInt8
@_silgen_name("switch_audio_playback_policy_reset")
private func playbackPolicyReset(_ handle: UnsafeMutableRawPointer, _ session: UInt8)
@_silgen_name("switch_audio_playback_policy_generation")
private func playbackPolicyGeneration(_ handle: UnsafeMutableRawPointer) -> UInt64
@_silgen_name("switch_audio_playback_policy_queued")
private func playbackPolicyQueued(_ handle: UnsafeMutableRawPointer) -> UInt8
@_silgen_name("switch_audio_playback_policy_underruns")
private func playbackPolicyUnderruns(_ handle: UnsafeMutableRawPointer) -> UInt64

/// Queue-confined Rust policy adapter. AVAudioPCMBuffer ownership and player
/// commands stay in the caller; none of these calls waits on the daemon.
final class AudioPlaybackPolicy {
    enum Arrival: UInt8 { case hold = 0, schedule = 1, start = 2, drop = 3 }
    enum Completion: UInt8 { case ignore = 0, counted = 1, pause = 2 }

    private let handle: UnsafeMutableRawPointer

    init() {
        guard let handle = playbackPolicyCreate() else {
            preconditionFailure("Rust playback policy allocation failed")
        }
        self.handle = handle
    }

    deinit { playbackPolicyDestroy(handle) }

    var generation: UInt64 { playbackPolicyGeneration(handle) }
    var queued: Int { Int(playbackPolicyQueued(handle)) }
    var underruns: UInt64 { playbackPolicyUnderruns(handle) }

    func receive() -> Arrival {
        Arrival(rawValue: playbackPolicyReceive(handle)) ?? .drop
    }

    func played(generation: UInt64, running: Bool) -> Completion {
        Completion(rawValue: playbackPolicyPlayed(handle, generation, running ? 1 : 0)) ?? .ignore
    }

    func resetForRoute() { playbackPolicyReset(handle, 0) }
    func resetForSession() { playbackPolicyReset(handle, 1) }
}
