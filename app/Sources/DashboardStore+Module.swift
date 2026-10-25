import Foundation

extension DashboardStore {
    // MARK: - DJI 模块 VoWiFi
    var voWiFiStatus: VoWiFiStatus? { moduleVoWiFi.state.status }
    var voWiFiLoaded: Bool { moduleVoWiFi.state.loaded }
    var voWiFiError: String? { moduleVoWiFi.state.error }
    var voWiFiActionInFlight: Bool { moduleVoWiFi.state.actionInFlight }
    var voWiFiStateRevision: UInt64 { moduleVoWiFi.state.revision }
}
