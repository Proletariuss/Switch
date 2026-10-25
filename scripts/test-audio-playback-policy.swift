import Foundation

// This test compiles the production audio bridge without opening devices.
// Its diagnostics use a bounded log writer supplied by BackendProcess in App.
enum BoundedBackendLog {
    static func append(_ data: Data, to url: URL, maximumBytes: Int, archives: Int) throws {}
}

private func check(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
}

/// Exercises the production Swift FFI adapter with CoreAudio buffers replaced
/// by integer identities. The Rust policy owns every admission decision.
private final class MockPlayback {
    let policy = AudioPlaybackPolicy()
    var pending: [Int] = []
    var scheduled: [Int] = []
    var playCount = 0
    var pauseCount = 0

    func receive(_ identity: Int) {
        switch policy.receive() {
        case .hold:
            pending.append(identity)
        case .start:
            pending.append(identity)
            scheduled.append(contentsOf: pending)
            pending.removeAll()
            playCount += 1
        case .schedule:
            scheduled.append(identity)
        case .drop:
            break
        }
    }

    func played(generation: UInt64, running: Bool = true) {
        switch policy.played(generation: generation, running: running) {
        case .ignore: break
        case .counted: break
        case .pause: pauseCount += 1
        }
    }

    func resetForRoute() {
        policy.resetForRoute()
        pending.removeAll()
        scheduled.removeAll()
    }

    func resetForSession() {
        policy.resetForSession()
        pending.removeAll()
        scheduled.removeAll()
    }
}

@main private enum PlaybackPolicyHarness {
    static func main() {
        let playback = MockPlayback()
        let firstGeneration = playback.policy.generation
        for id in 0..<5 { playback.receive(id) }
        check(playback.scheduled.isEmpty && playback.playCount == 0, "started before 120 ms")
        playback.receive(5)
        check(playback.scheduled == Array(0..<6) && playback.playCount == 1, "lost prime order")
        for id in 6..<21 { playback.receive(id) }
        check(playback.scheduled == Array(0..<20) && playback.policy.queued == 20,
              "20-buffer cap did not drop the 21st buffer")
        for _ in 0..<20 { playback.played(generation: firstGeneration) }
        check(playback.pauseCount == 1 && playback.policy.underruns == 1, "underrun did not pause")

        for id in 21..<28 { playback.receive(id) }
        check(playback.playCount == 1, "restarted before 160 ms after underrun")
        playback.receive(28)
        check(playback.playCount == 2 && playback.policy.queued == 8, "160 ms reprime failed")
        playback.resetForRoute()
        playback.played(generation: firstGeneration)
        check(playback.pauseCount == 1 && playback.policy.queued == 0,
              "late old-route completion changed a new route")

        let secondGeneration = playback.policy.generation
        for id in 29..<36 { playback.receive(id) }
        check(playback.playCount == 2, "route reset lost the adaptive threshold")
        playback.receive(36)
        check(playback.playCount == 3, "new route did not reprime")
        playback.played(generation: secondGeneration, running: false)
        check(playback.policy.queued == 8, "stopped playback consumed a completion")
        playback.resetForSession()
        playback.played(generation: secondGeneration)
        check(playback.policy.queued == 0 && playback.policy.underruns == 0,
              "late old-session completion revived playback")
        for id in 37..<42 { playback.receive(id) }
        check(playback.playCount == 3, "new session did not reset to 120 ms")
        playback.receive(42)
        check(playback.playCount == 4 && playback.policy.queued == 6,
              "new session 120 ms start failed")
        print("PASS: Swift/Rust playback policy prime, cap, underrun and stale generation")

        let token = Data("session-secret\0".utf8)
        let pcm = Data(repeating: 0x5a, count: 640)
        var ringback = true
        var playbackAttempts = 0
        var conversionAccepted = false
        var heldFrames = 0
        let remotePolicy = AudioPlaybackPolicy()
        var remoteArrivals: [AudioPlaybackPolicy.Arrival] = []
        var malformedFrames: [Int] = []
        func receive(_ packet: Data?) {
            if ReaderRemotePCMFrame.accepted(packet, token: token, frameSamples: 320,
                                             rejectMalformed: { malformedFrames.append($0) }, play: { frame in
                playbackAttempts += 1
                check(Data(frame) == pcm, "admitted PCM bytes changed")
                guard conversionAccepted else { return false }
                let arrival = remotePolicy.receive()
                remoteArrivals.append(arrival)
                if arrival == .hold { heldFrames += 1 }
                return ReaderRemotePCMFrame.beginsPlayback(arrival)
            }) {
                ringback = false
            }
        }
        receive(token + pcm.dropLast(2))
        receive(token + pcm + Data([0, 0]))
        receive(Data("other-session\0".utf8) + pcm)
        check(ringback && playbackAttempts == 0 && malformedFrames == [638, 642],
              "malformed or unauthenticated downlink stopped local ringback")
        receive(token + pcm)
        check(ringback && playbackAttempts == 1 && remotePolicy.queued == 0,
              "conversion rejection stopped ringback or reached playback policy")
        conversionAccepted = true
        receive(token + pcm)
        // A stalled stream after one valid frame must leave the local tone on.
        check(ringback && heldFrames == 1 && remotePolicy.queued == 0,
              "first priming frame stopped ringback before playback started")
        receive(token + pcm + Data([0, 0]))
        check(ringback && heldFrames == 1 && remotePolicy.queued == 0,
              "oversized media advanced playback priming")
        for _ in 0..<4 { receive(token + pcm) }
        check(ringback && heldFrames == 5 && remotePolicy.queued == 0,
              "held PCM frames stopped local ringback before the prime threshold")
        receive(token + pcm)
        check(!ringback && remoteArrivals == Array(repeating: .hold, count: 5) + [.start]
              && remotePolicy.queued == 6,
              "sixth authenticated PCM frame did not start playback and stop ringback")
        receive(token + pcm)
        check(remoteArrivals.last == .schedule && remotePolicy.queued == 7,
              "subsequent valid PCM was not scheduled")
        check(!ReaderRemotePCMFrame.beginsPlayback(.drop),
              "dropped playback frame was treated as audible")
        print("PASS: remote PCM authentication, malformed frames, stalled priming and ringback")
    }
}
