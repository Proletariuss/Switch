import Foundation

extension DashboardStore {
    // MARK: - 读卡器（PC/SC、SIM 身份与 VoWiFi）
    var readerList: [String] { readerRuntime.readers }
    var readerListLoaded: Bool { readerRuntime.listLoaded }
    var readerConnected: Bool { readerRuntime.connected }
    var readerName: String? { readerRuntime.name }
    var readerUSIMInfo: USIMInfo? { readerRuntime.usimInfo }
    var readerProfileOperatorName: String? { readerRuntime.profileOperatorName }
    var readerEPDGFQDN: String? { readerRuntime.epdgFQDN }
    var readerCarrierConfigError: String? { readerRuntime.carrierConfigError }
    var readerHasESIM: Bool { readerRuntime.hasESIM }
    var readerCardType: String? { readerRuntime.cardType }
    var readerError: String? { readerRuntime.error }
    var readerActionInFlight: Bool { readerRuntime.actionInFlight }
    var readerVoWiFiStatus: ReaderVoWiFiStatus? { readerRuntime.voWiFiStatus }
    var readerVoWiFiLoaded: Bool { readerRuntime.voWiFiLoaded }
    var readerVoWiFiActionInFlight: Bool { readerRuntime.voWiFiActionInFlight }
    var readerVoWiFiProxyIP: String {
        get { readerRuntime.proxyIP }
        set { readerRuntime.proxyIP = newValue }
    }
    var readerVoWiFiProxyPort: String {
        get { readerRuntime.proxyPort }
        set { readerRuntime.proxyPort = newValue }
    }
    var readerVoWiFiEPDG: String {
        get { readerRuntime.customEPDG }
        set { readerRuntime.customEPDG = newValue }
    }
    var readerVoWiFiEPDGPort: String {
        get { readerRuntime.customEPDGPort }
        set { readerRuntime.customEPDGPort = newValue }
    }
    var proxyCheckResult: ProxyCheckResult? { readerRuntime.proxyCheckResult }
    var proxyChecking: Bool { readerRuntime.proxyChecking }
    var readerVoWiFiEPDGError: String? { readerRuntime.epdgError }
}
