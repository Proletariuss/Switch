import AppKit
import SwiftUI

@MainActor enum ReaderPresentationFixture {
    static var profileCount = 2
    static var identityPending = false
    static var calls: [String] = []
    static let targetICCID = "8944000000000000001"
    static let phone = "+447700900123"
    static let longName = "英国工作与旅行备用号码 · 长名称可读性验证 Long personalised eSIM profile name"

    static func decode<T: Decodable>(_ type: T.Type, _ json: String) -> T {
        try! JSONDecoder().decode(type, from: Data(json.utf8))
    }
}

// No real service, URLSession, Keychain, notification centre, PC/SC or BLE
// manager is linked. Actions record intent only and never touch user settings.
@MainActor final class DashboardStore: ObservableObject {
    @Published var readerConnected = true
    @Published var readerName: String? = "ESTKme Technology Limited ESTKme-RED · fixture reader"
    @Published var readerUSIMInfo: USIMInfo?
    @Published var readerHasESIM = true
    @Published var readerList = ["ESTKme Technology Limited ESTKme-RED · fixture reader"]
    @Published var readerListLoaded = true
    @Published var readerActionInFlight = false
    @Published var readerVoWiFiActionInFlight = false
    @Published var readerVoWiFiLoaded = true
    @Published var readerVoWiFiStatus: ReaderVoWiFiStatus? = ReaderPresentationFixture.decode(
        ReaderVoWiFiStatus.self, #"{"available":true,"tunnel_state":"disconnected"}"#)
    @Published var readerError: String?
    var readerOperatorDisplayName: String { readerUSIMInfo?.operatorDisplayName ?? "身份待确认" }
    init() {
        if !ReaderPresentationFixture.identityPending {
            readerUSIMInfo = ReaderPresentationFixture.decode(USIMInfo.self, """
                {"iccid":"\(ReaderPresentationFixture.targetICCID)","imsi":"234000000000001","mcc":"234","mnc":"33",
                 "phone_number":"\(ReaderPresentationFixture.phone)","operator_name":"CTExcel · offline fixture",
                 "private_identity":"fixture@ims.example.invalid","home_domain":"ims.example.invalid"}
                """)
        }
    }
    func notifyCopied() { ReaderPresentationFixture.calls.append("copy") }
    func connectReader(_ name: String) { ReaderPresentationFixture.calls.append("connect") }
    func disconnectReader() { ReaderPresentationFixture.calls.append("disconnect") }
    func clearReaderRuntimeState(clearList: Bool = false) { ReaderPresentationFixture.calls.append("clear") }
    func refreshReaderList(autoConnect: Bool = true) { ReaderPresentationFixture.calls.append("scan") }
    func refreshSIM() { ReaderPresentationFixture.calls.append("refreshSIM") }
}

@MainActor final class ReaderESIMStore: ObservableObject {
    struct LegacyRecoveryCandidate: Identifiable {
        let snapshot: ESIMOperationSnapshot
        let rawUpdatedAt: String
        var id: String { snapshot.id }
    }
    struct ReaderContext: Equatable { let name: String?; let connected: Bool; let hasESIM: Bool }
    enum LoadPhase: Equatable { case idle, loading, loaded, failed(String) }
    enum Action: Equatable { case switching(String), deleting(String), renaming(String), downloading }
    @Published var overview: ESIMOverview?
    @Published var banner: ESIMBanner?
    @Published var operation: ESIMOperationSnapshot?
    @Published var legacyRecovery: LegacyRecoveryCandidate?
    @Published var legacyRecoveryNotice: String?
    @Published var legacyRecoveryError: String?
    @Published var legacyRecoveryStatusFresh = false
    @Published var isAcknowledgingLegacyRecovery = false
    @Published var notifications: [ESIMNotificationItem] = []
    @Published var loadPhase = LoadPhase.loaded
    @Published var action: Action?
    @Published var isRefreshing = false
    let isBusy = false
    let isInitialLoading = false
    let isPhysicalSIM = false
    var capabilities: ESIMCapabilities? { overview?.capabilities }
    var profileGroups: [(group: EUICCProfiles, profiles: [ESIMProfileContext])] {
        (overview?.profiles ?? []).map { group in
            (group, (group.profiles ?? []).map { ESIMProfileContext(eid: group.eid ?? "", aidHex: group.aidHex ?? "", profile: $0) })
        }
    }
    var profileCount: Int { profileGroups.reduce(0) { $0 + $1.profiles.count } }
    init() {
        let profiles: [[String: Any]] = (1...ReaderPresentationFixture.profileCount).map { index in
            ["iccid": String(format: "894400000000000%04d", index),
             "name": index == 1 ? ReaderPresentationFixture.longName : "备用号卡 \(index) · Personal line",
             "service_provider_name": "CTExcel", "state": index == 1 ? 1 : 0]
        }
        let json: [String: Any] = ["card_type": "euicc", "profiles": [["eid": "fixture-eid", "aid_hex": "A000", "profiles": profiles]],
            "capabilities": ["can_refresh": true, "can_download": true, "can_switch": true, "can_rename": true,
                             "can_delete": true, "can_probe_phonebook": false, "vendor_serial_available": false]]
        overview = try! JSONDecoder().decode(ESIMOverview.self, from: JSONSerialization.data(withJSONObject: json))
    }
    func synchronizeReader(_ context: ReaderContext) -> Bool {
        ReaderPresentationFixture.calls.append("synchronize")
        return false // Fixture represents an already-loaded, unchanged reader.
    }
    func savedPhoneNumber(for iccid: String) -> String? { ReaderPresentationFixture.phone }
    func isProfileActionRunning(_ context: ESIMProfileContext) -> Bool { false }
    func refresh() { ReaderPresentationFixture.calls.append("refresh") }
    func load() { ReaderPresentationFixture.calls.append("load") }
    func loadNotifications() { ReaderPresentationFixture.calls.append("notifications") }
    func observeLegacyRecovery() async {}
    func refreshLegacyRecovery() async {}
    func acknowledgeLegacyRecovery(
        _ reviewed: LegacyRecoveryCandidate,
        reviewedByUser: Bool,
        enteredIDSuffix: String
    ) async -> Bool { false }
    func switchProfile(_ context: ESIMProfileContext) { ReaderPresentationFixture.calls.append("switch") }
    func deleteProfile(_ context: ESIMProfileContext) { ReaderPresentationFixture.calls.append("delete") }
}

enum BluetoothReaderErrorCode: String { case unauthorized }
@MainActor final class BluetoothReaderService: ObservableObject {
    @Published var connectedID: UUID?
    @Published var devices: [UUID: String] = [:]
    @Published var errorCode: BluetoothReaderErrorCode?
    let isSearching = false
    let statusText = "Offline fixture"
    func displayName(forReader reader: String) -> String { reader }
    func nineEsimPassword(for id: UUID) throws -> String? { nil }
    func saveNineEsimPassword(_ password: String?, for id: UUID) throws {}
    func supportsNineEsimCredentials(_ id: UUID) -> Bool { false }
    func scan() { ReaderPresentationFixture.calls.append("BLE scan") }
    func stopScan() { ReaderPresentationFixture.calls.append("BLE stop") }
    func connect(_ id: UUID) { ReaderPresentationFixture.calls.append("BLE connect") }
    func disconnect() { ReaderPresentationFixture.calls.append("BLE disconnect") }
}

// Closed sheets are outside this page-layout test. Their launching closures
// remain the real production code; accidental presentation is detected above.
struct ReaderRenameProfileView: View {
    let context: ESIMProfileContext; let store: ReaderESIMStore
    var body: some View { Text("Offline rename sheet") }
}
struct ReaderDownloadESIMView: View {
    let store: ReaderESIMStore
    var body: some View { Text("Offline download sheet") }
}
struct ReaderESIMNotificationsView: View {
    let store: ReaderESIMStore; let privacyMaskEnabled: Bool
    var body: some View { Text("Offline notification sheet") }
}

struct ReaderCommunicationFixture: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("Wi-Fi Calling", systemImage: "wifi").font(.headline)
            Text("离线布局测试 · 不建立真实连接").font(.caption).foregroundStyle(.secondary)
            Text("SIM → 接入 → 隧道 → IMS → SMS").font(.caption)
            TextField("自动使用系统网络", text: .constant("")).textFieldStyle(.workspace)
            Button("开启 Wi-Fi Calling") {}.buttonStyle(.proletariusPrimary)
        }
        .padding(16)
        .frame(maxWidth: .infinity, minHeight: 210, alignment: .topLeading)
        .workspaceCard()
    }
}
