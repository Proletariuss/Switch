import Foundation

extension DashboardStore {
    // MARK: - 模块网络与设备健康状态
    var traffic: TrafficSnapshot? { moduleNetwork.traffic }
    var health: HealthStatus? { moduleState.health }
    var status: DeviceStatus? { moduleState.status }
    var lastUpdated: Date? { moduleState.capturedAt }
    var statusStale: Bool { moduleState.isStale }
    var readPath: String { moduleState.readPath }
    var rustSnapshotComplete: Bool? { moduleState.rustSnapshotComplete }
    var rustSnapshotIssues: [DeviceSnapshotIssue] { moduleState.rustSnapshotIssues }
    var rustSnapshotError: String? { moduleState.rustSnapshotError }
    var deviceStateRevision: UInt64 { moduleState.revision }
    var usbnetEnabled: Bool { moduleNetwork.usbnetEnabled }
    var usbnetLoaded: Bool { moduleNetwork.usbnetLoaded }
    var busy: Bool { moduleNetwork.busy }
    var networkRecovering: Bool { moduleNetwork.recovering }
    var networkRecoveryMessage: String? { moduleNetwork.recoveryMessage }
    var check4G: CheckResult? { moduleNetwork.check4G }
}
