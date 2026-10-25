import Foundation

@_silgen_name("switch_audio_reader_admission")
private func rustReaderAudioAdmission(_ snapshot: UnsafePointer<UInt8>?, _ length: Int) -> UInt8

/// The Rust core chooses reader audio intent from a bounded status snapshot.
/// Call identity, hangup fencing and every macOS audio side effect stay here.
enum ReaderAudioAdmission {
    enum Intent: Equatable {
        case closed
        case receiveOnly
        case receiveWithLocalRingback
        case incomingReceiveOnly
        case duplex

        var isAdmitted: Bool { self != .closed }
        var isReceiveOnly: Bool {
            self == .receiveOnly || self == .receiveWithLocalRingback || self == .incomingReceiveOnly
        }
        var captureMicrophone: Bool { self == .duplex }
        var localRingback: Bool { self == .receiveWithLocalRingback }
        var isIncomingReceiveOnly: Bool { self == .incomingReceiveOnly }
        var mayRecoverRoute: Bool { self == .incomingReceiveOnly || self == .duplex }
    }

    static func decide(status: CallStatus?, call: ReaderVoWiFiCallState?) -> Intent {
        guard let status, let call else { return .closed }
        let endpointReady: Bool
        if let port = call.audioPort, let token = call.audioToken {
            endpointReady = (1...65535).contains(port) && !token.isEmpty
        } else {
            endpointReady = false
        }
        let display: UInt8 = switch status.state {
            case "dialing": 1
            case "alerting": 2
            case "active": 3
            default: 0
        }
        let raw: UInt8 = switch call.state {
            case "early": 1
            case "connecting": 2
            case "active": 3
            default: 0
        }
        let direction: UInt8 = switch call.direction {
            case "incoming": 1
            case "outgoing": 2
            default: 0
        }
        let snapshot: [UInt8] = [
            display,
            raw,
            direction,
            call.signalingReady ? 1 : 0,
            call.mediaReady ? 1 : 0,
            call.earlyMedia ? 1 : 0,
            endpointReady ? 1 : 0,
        ]
        let wire = snapshot.withUnsafeBufferPointer {
            rustReaderAudioAdmission($0.baseAddress, $0.count)
        }
        switch wire {
        case 1: return .receiveOnly
        case 2: return .receiveWithLocalRingback
        case 3: return .incomingReceiveOnly
        case 4: return .duplex
        default: return .closed
        }
    }
}
