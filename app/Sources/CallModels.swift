import Foundation

// MARK: - 语音与通话

struct CallStatus: Decodable, Equatable {
    let state: String
    let number: String?
    let incoming: Bool?
    let active: Bool?
    var historyError: String? = nil

    enum CodingKeys: String, CodingKey {
        case state, number, incoming, active
        case historyError = "history_error"
    }

    var isIdle: Bool { state == "idle" }
    var isIncoming: Bool { state == "incoming" }
    var isActive: Bool { state == "active" }
}

struct CallDialRequest: Encodable {
    let clientRequestID: String
    let number: String
    let preferredOwner: String?
    let observedOwner: String?
    let generation: UInt64

    enum CodingKeys: String, CodingKey {
        case number, generation
        case clientRequestID = "client_request_id"
        case preferredOwner = "preferred_owner"
        case observedOwner = "observed_owner"
    }

    init(clientRequestID: String, number: String, preferredOwner: String? = nil, observedOwner: String? = nil,
         generation: UInt64 = 0) {
        self.clientRequestID = clientRequestID
        self.number = number
        self.preferredOwner = preferredOwner
        self.observedOwner = observedOwner
        self.generation = generation
    }
}

struct CallDTMFRequest: Encodable {
    let clientRequestID: String
    let digit: String
    let preferredOwner: String?
    let observedOwner: String?
    let generation: UInt64

    enum CodingKeys: String, CodingKey {
        case digit, generation
        case clientRequestID = "client_request_id"
        case preferredOwner = "preferred_owner"
        case observedOwner = "observed_owner"
    }

    init(clientRequestID: String, digit: String, preferredOwner: String? = nil, observedOwner: String? = nil,
         generation: UInt64 = 0) {
        self.clientRequestID = clientRequestID
        self.digit = digit
        self.preferredOwner = preferredOwner
        self.observedOwner = observedOwner
        self.generation = generation
    }
}

struct CallControlRequest: Encodable {
    let clientRequestID: String
    let preferredOwner: String?
    let observedOwner: String?
    let generation: UInt64
    let command: String?

    enum CodingKeys: String, CodingKey {
        case generation, command
        case clientRequestID = "client_request_id"
        case preferredOwner = "preferred_owner"
        case observedOwner = "observed_owner"
    }
}

struct CallActionResult: Decodable {
    let accepted: Bool
    let response: String?
    let warning: String?
}

struct ReaderVoWiFiCallState: Decodable, Equatable {
    let state: String
    let direction: String?
    let peer: String?
    let callID: String?
    let codec: String?
    let startedAt: String?
    let lastError: String?
    let mediaReady: Bool
    let earlyMedia: Bool
    let signalingReady: Bool
    let audioPort: Int?
    let audioToken: String?
    let rtpPacketsSent: UInt64?
    let rtpPacketsReceived: UInt64?
    let rtpDecodeErrors: UInt64?
    let audioFramesToHost: UInt64?

    enum CodingKeys: String, CodingKey {
        case state, direction, peer, codec
        case callID = "call_id"
        case startedAt = "started_at"
        case lastError = "last_error"
        case mediaReady = "media_ready"
        case earlyMedia = "early_media"
        case signalingReady = "signaling_ready"
        case audioPort = "audio_port"
        case audioToken = "audio_token"
        case rtpPacketsSent = "rtp_packets_sent"
        case rtpPacketsReceived = "rtp_packets_received"
        case rtpDecodeErrors = "rtp_decode_errors"
        case audioFramesToHost = "audio_frames_to_host"
    }
}

/// A reader INVITE is actionable from the moment its own signalling state is
/// ready.  The broader VoWiFi status is refreshed on a slower cadence and is
/// useful for presentation and outbound-call admission, but must not make an
/// already-delivered incoming INVITE impossible to answer.
enum ReaderCallReadiness {
    static func canAnswer(_ state: ReaderVoWiFiCallState?) -> Bool {
        guard let state else { return false }
        return state.state == "incoming" && state.signalingReady
    }
}

struct ReaderVoWiFiCallResponse: Decodable {
    let generation: UInt64
    let source: String?
    let call: ReaderVoWiFiCallState
    var historyError: String? = nil

    enum CodingKeys: String, CodingKey {
        case generation, source, call
        case historyError = "history_error"
    }
}

struct ReaderVoWiFiCallRequest: Encodable {
    let peer: String?
    let generation: UInt64
}

struct ReaderVoWiFiDTMFRequest: Encodable {
    let digit: String
    let generation: UInt64
}

struct CallClearResult: Decodable {
    let ok: Bool
}

struct CallDeleteRequest: Encodable {
    let id: String
}

// MARK: - 通话记录

/// GET /api/calls：顶层是数组
struct CallRecord: Decodable, Identifiable {
    let id: String
    let device: String?
    let direction: String
    let number: String?
    let answered: Bool
    let startedAt: Date
    let endedAt: Date
    let duration: Int

    var isIncoming: Bool { direction == "in" }
    var isMissed: Bool { isIncoming && !answered }
    var deviceName: String {
        switch device {
        case "reader": return "读卡器 VoWiFi"
        default: return "DJI 模块"
        }
    }

    enum CodingKeys: String, CodingKey {
        case id, device, direction, number, answered, duration
        case startedAt = "started_at"
        case endedAt = "ended_at"
    }
}

struct VoiceEnableRequest: Encodable {
    let enabled: Bool
}

struct VoiceEnableResult: Decodable {
    let accepted: Bool
    let enabled: Bool
    let needsUnplug: Bool?

    enum CodingKeys: String, CodingKey {
        case accepted, enabled
        case needsUnplug = "needs_unplug"
    }
}

struct VoiceEnabledResponse: Decodable {
    let enabled: Bool
}

struct CallModeDownloadSource: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let detail: String
    let trusted: Bool
}

struct CallModeRestoreNotice: Decodable, Equatable {
    let backupID: String
    let backupSavedAt: Date
    let restoredAt: Date
    let safetyBackupID: String?
    let changed: Bool

    enum CodingKeys: String, CodingKey {
        case changed
        case backupID = "backup_id"
        case backupSavedAt = "backup_saved_at"
        case restoredAt = "restored_at"
        case safetyBackupID = "safety_backup_id"
    }
}

struct CallModeStatus: Decodable, Equatable {
    let state: String
    let summary: String
    let detail: String?
    let adbEnabled: Bool
    let interfacesReady: Bool
    let adbAuthorizationRequired: Bool
    let uacEnabled: Bool
    let voiceConfigured: Bool
    let runtimeDownloaded: Bool
    let runtimeVersion: String
    let runtimePath: String
    let downloadedBytes: Int64
    let totalBytes: Int64
    let source: String?
    let canEnable: Bool
    let canDownload: Bool
    let canRetry: Bool
    let requiresADBAuthorizationConfirmation: Bool
    let requiresUSBConfirmation: Bool
    let requiresDownloadConfirmation: Bool
    let backupPath: String?
    let lastRestore: CallModeRestoreNotice?
    let updatedAt: Date
    let sources: [CallModeDownloadSource]
    let upstream: String

    var isReady: Bool { state == "ready" }
    var isBusy: Bool {
        [
            "authorizing_adb", "enabling_usb", "enabling_voice", "restarting", "verifying_usb",
            "downloading", "preparing", "restoring_usb", "restarting_restore",
        ].contains(state)
    }
    var downloadProgress: Double {
        guard totalBytes > 0 else { return 0 }
        return min(1, max(0, Double(downloadedBytes) / Double(totalBytes)))
    }

    enum CodingKeys: String, CodingKey {
        case state, summary, detail, source, sources, upstream
        case adbEnabled = "adb_enabled"
        case interfacesReady = "interfaces_ready"
        case adbAuthorizationRequired = "adb_authorization_required"
        case uacEnabled = "uac_enabled"
        case voiceConfigured = "voice_configured"
        case runtimeDownloaded = "runtime_downloaded"
        case runtimeVersion = "runtime_version"
        case runtimePath = "runtime_path"
        case downloadedBytes = "downloaded_bytes"
        case totalBytes = "total_bytes"
        case canEnable = "can_enable"
        case canDownload = "can_download"
        case canRetry = "can_retry"
        case requiresADBAuthorizationConfirmation = "requires_adb_authorization_confirmation"
        case requiresUSBConfirmation = "requires_usb_confirmation"
        case requiresDownloadConfirmation = "requires_download_confirmation"
        case backupPath = "backup_path"
        case lastRestore = "last_restore"
        case updatedAt = "updated_at"
    }
}

struct CallModeUSBBackupSummary: Decodable, Identifiable, Equatable {
    let id: String
    let fileName: String
    let schemaVersion: Int
    let savedAt: Date
    let reason: String
    let moduleIMEI: String?
    let firmware: String?
    let vendorID: String?
    let productID: String?
    let flags: [Int]?
    let adbEnabled: Bool
    let uacEnabled: Bool
    let voiceIncluded: Bool
    let imsConfigured: Bool
    let imported: Bool
    let valid: Bool
    let restorable: Bool
    let detail: String?

    enum CodingKeys: String, CodingKey {
        case id, reason, flags, imported, valid, restorable, detail, firmware
        case fileName = "file_name"
        case schemaVersion = "schema_version"
        case savedAt = "saved_at"
        case moduleIMEI = "module_imei"
        case vendorID = "vendor_id"
        case productID = "product_id"
        case adbEnabled = "adb_enabled"
        case uacEnabled = "uac_enabled"
        case voiceIncluded = "voice_included"
        case imsConfigured = "ims_configured"
    }
}

struct CallModeUSBBackupListResponse: Decodable {
    let backups: [CallModeUSBBackupSummary]
}

struct CallModeUSBBackupImportResponse: Decodable {
    let backup: CallModeUSBBackupSummary
}

struct CallModeUSBBackupDeleteResponse: Decodable {
    let deleted: Bool
    let backupID: String

    enum CodingKeys: String, CodingKey {
        case deleted
        case backupID = "backup_id"
    }
}

struct CallModeEnableRequest: Encodable {
    let confirm: Bool
    let confirmADBAuthorization: Bool

    enum CodingKeys: String, CodingKey {
        case confirm
        case confirmADBAuthorization = "confirm_adb_authorization"
    }
}

struct CallModeDownloadRequest: Encodable {
    let confirm: Bool
    let source: String
}

struct CallModeRestoreRequest: Encodable {
    let confirm: Bool
    let backupID: String

    enum CodingKeys: String, CodingKey {
        case confirm
        case backupID = "backup_id"
    }
}

struct CallModeBackupDeleteRequest: Encodable {
    let confirm: Bool
    let backupID: String

    enum CodingKeys: String, CodingKey {
        case confirm
        case backupID = "backup_id"
    }
}

struct CallAudioStartResult: Decodable {
    let started: Bool
    let runtimeVersion: String

    enum CodingKeys: String, CodingKey {
        case started
        case runtimeVersion = "runtime_version"
    }
}

struct CallAudioStopResult: Decodable {
    let stopped: Bool
}
