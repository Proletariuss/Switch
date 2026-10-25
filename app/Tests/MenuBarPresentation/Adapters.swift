import AppKit
import SwiftUI

@MainActor
enum MenuBarFixture {
    static let defaults = UserDefaults(suiteName: "MenuBarPresentation-\(UUID().uuidString)")!
    static var actions: [String] = []
}

enum AppSceneID { static let mainWindow = "main" }
struct QmiDeviceStatus {
    var enabled: Bool
    var sessionInitialized: Bool
    var initializationError: String? = nil
}
struct DeviceStatus {
    var hardwareStatus: String?
    var usbDevice: String? = "fixture"
    var imei: String? = "fixture-imei"
    var iccid: String? = "fixture-iccid"
    var imsi: String? = "fixture-imsi"
    var simInserted: Bool? = true
    var operatorName: String? = "示例运营商"
    var phoneNumber: String? = "+8613800138000"
    var networkMode: String? = "5G"
    var signalDbm: Int? = -72
    var regStatus: Int? = 1
    var radioBand: String? = "n78"
    var qmi: QmiDeviceStatus? = nil
}
struct TrafficSnapshot {
    var available = true
    var sessionRX: UInt64? = 12_345_678
    var sessionTX: UInt64? = 1_234_567
}
struct ReaderUSIMInfo {
    var iccid = "fixture"
    var mcc = "460"
    var phoneNumber: String? = "+8613900139000"
    var operatorDisplayName = "读卡器示例运营商"
}
struct VoWiFiStatus {
    var state = "ready"
    var vowifiActive = true
    var imsRegistered = true
    var smsReady: Bool? = true
    var callReady: Bool? = true
}
struct ReaderVoWiFiStatus {
    var imsState = "registered"
    var tunnelState = "connected"
    var smsReady: Bool? = true
    var callReady: Bool? = true
}
struct CallStatus { var state = "idle" }

@MainActor final class BackendProcess: ObservableObject {
    enum State: Equatable { case running, starting, stopping, stopped, failed(String) }
    @Published var state = State.running
}
@MainActor final class DashboardStore: ObservableObject {
    @Published var status: DeviceStatus? = DeviceStatus()
    @Published var statusStale = false
    @Published var readerConnected = true
    @Published var readerUSIMInfo: ReaderUSIMInfo? = ReaderUSIMInfo()
    @Published var readerOperatorDisplayName = "读卡器示例运营商"
    @Published var readerCardType = "euicc"
    @Published var voWiFiLoaded = true
    @Published var voWiFiStatus: VoWiFiStatus? = VoWiFiStatus()
    @Published var voWiFiActionInFlight = false
    @Published var readerVoWiFiLoaded = true
    @Published var readerVoWiFiStatus: ReaderVoWiFiStatus? = ReaderVoWiFiStatus()
    @Published var callStatus = CallStatus()
    @Published var traffic: TrafficSnapshot? = TrafficSnapshot()
    @Published var lastUpdated: Date? = Date(timeIntervalSince1970: 1_790_758_800)
    @Published var networkRecovering = false
    func refresh() { MenuBarFixture.actions.append("refresh") }
    func notifyCopied() { MenuBarFixture.actions.append("copy") }
}
@MainActor final class SMSStore: ObservableObject { @Published var items = [1, 2, 3] }
@MainActor final class AttentionStore: ObservableObject {
    @Published var unreadSMSCount = 2
    @Published var unviewedCallCount = 1
    var totalCount: Int { unreadSMSCount + unviewedCallCount }
    var hasAttention: Bool { totalCount > 0 }
    var accessibilitySummary: String? { "未读短信 \(unreadSMSCount)，未查看电话 \(unviewedCallCount)" }
    static func compactCount(_ count: Int) -> String { count > 99 ? "99+" : String(max(0, count)) }
}
@MainActor final class UpdateChecker: ObservableObject {
    struct Release { var tagName = "fixture" }
    @Published var pendingUpdate: Release?
    var downloadURL: URL? { nil }
    func dismissUpdate() { MenuBarFixture.actions.append("dismissUpdate") }
}
@MainActor enum AutoLaunch {
    static var isEnabled: Bool { false }
    static let didChangeNotification = Notification.Name("fixture-autoLaunch")
    static func setEnabled(_ enabled: Bool) throws { MenuBarFixture.actions.append("autoLaunch") }
}
func iccidFlagEmoji(_ iccid: String?) -> String? { nil }
func mccFlagEmoji(_ mcc: String?) -> String? { "🇨🇳" }
