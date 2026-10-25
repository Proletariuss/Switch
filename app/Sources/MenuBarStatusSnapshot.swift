import Foundation
import Combine

/// Presentation rules shared by the panel and its offline regression tests.
/// A running local service does not imply that either device is online.
enum MenuBarStatusPolicy {
    static func moduleSIMLabel(_ inserted: Bool?) -> String {
        switch inserted {
        case true: return "SIM 已插入"
        case false: return "SIM 未插入"
        case nil: return "SIM 状态待确认"
        }
    }

    /// USB enumeration proves presence only. A live control response must
    /// provide at least one value returned by the modem's AT/QMI status path.
    static func moduleHasControlResponse(imei: String?, iccid: String?, imsi: String?,
                                         operatorName: String?, networkMode: String?,
                                         simInserted: Bool?, signalDbm: Int?,
                                         registrationStatus: Int?) -> Bool {
        let identifiers = [imei, iccid, imsi, operatorName, networkMode]
        return identifiers.contains { value in
            guard let value else { return false }
            return !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        } || simInserted != nil || signalDbm != nil || registrationStatus != nil
    }

    static func moduleStatus(serviceRunning: Bool, hasStatus: Bool, stale: Bool,
                             hardwareIssue: Bool, hardwarePresent: Bool,
                             enumeratedDevicePresent: Bool = false,
                             simInserted: Bool?,
                             qmiSessionInitialized: Bool? = nil) -> String {
        guard serviceRunning else { return "状态未知" }
        if stale { return "数据过期" }
        guard hasStatus else { return "正在读取" }
        if qmiSessionInitialized == true { return "QMI 会话已初始化" }
        if hardwareIssue || !hardwarePresent {
            return enumeratedDevicePresent ? "已检测到，控制通道未就绪" : "未连接"
        }
        if simInserted == false { return "SIM 未插入" }
        return "已连接"
    }

    static func imsStatus(loaded: Bool, failed: Bool, registered: Bool,
                          smsReady: Bool?, callReady: Bool?, connecting: Bool,
                          requiresCall: Bool = true) -> String {
        guard loaded else { return "读取中" }
        if failed { return "异常" }
        if registered {
            return smsReady == true && (!requiresCall || callReady == true) ? "已就绪" : "恢复中"
        }
        return connecting ? "连接中" : "未启用"
    }

    static func callStatus(serviceRunning: Bool, state: String) -> String {
        guard serviceRunning else { return "状态未知" }
        switch state {
        case "incoming": return "有来电"
        case "active": return "通话中"
        case "dialing": return "正在拨号"
        case "alerting": return "等待接听"
        case "idle": return "空闲"
        default: return "状态未知"
        }
    }
}
