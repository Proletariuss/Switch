import Foundation

// MARK: - AT

struct ATRequest: Encodable {
    let command: String

    // Display hints only. The backend remains the command/ownership boundary;
    // an absent hint never certifies an unfamiliar vendor command as read-only.
    var impactDescriptions: [String] {
        var tokens: [String] = []
        var token = ""
        var quoted = false
        var escaped = false
        for character in command.uppercased() {
            if quoted {
                if escaped { escaped = false; continue }
                if character == "\\" { escaped = true; continue }
                if character == "\"" { quoted = false }
                continue
            }
            if character == "\"" { quoted = true; continue }
            if character == ";" {
                tokens.append(token)
                token = ""
            } else if character != " " {
                token.append(character)
            }
        }
        tokens.append(token)
        var descriptions: [String] = []
        func append(_ text: String) {
            if !descriptions.contains(text) { descriptions.append(text) }
        }
        for token in tokens {
            let body = token.hasPrefix("AT") ? String(token.dropFirst(2)) : token
            if body.hasSuffix("=?") { continue }
            switch body {
            case let value where value.hasPrefix("+CFUN="):
                append("更改模块工作模式，可能关闭射频、重启模块或中断当前连接。")
            case let value where value.hasPrefix("+COPS="):
                append("更改运营商选择，重新注册期间可能暂时断网。")
            case let value where value.hasPrefix("+CGATT="):
                append("更改蜂窝数据附着状态，可能断开数据连接。")
            case let value where value.hasPrefix("+CMGD="):
                append("删除当前选定存储中的短信；删除范围由命令参数决定。")
            case let value where value.hasPrefix("+CPMS="):
                append("切换后续短信命令使用的存储区域。")
            case let value where value.hasPrefix("+CMGS="):
                append("进入短信发送流程；此控制台不提供后续正文输入，请使用短信中心发送。")
            case let value where value.hasPrefix("D"):
                append("发起拨号，可能建立通话或数据连接并产生运营商费用。")
            case let value where value.hasPrefix("H"):
                append("结束当前模块通话或数据连接。")
            case let value where value.hasPrefix("Z"):
                append("恢复已保存的模块设置，可能中断当前连接。")
            case let value where value.hasPrefix("&F"):
                append("恢复模块出厂配置，可能更改网络和串口设置。")
            default: break
            }
        }
        return descriptions
    }
}

struct ATResult: Decodable {
    enum Outcome: String, Decodable {
        case success
        case moduleError = "module_error"
    }

    let response: String?
    let outcome: Outcome?
    let errorKind: String?

    init(response: String?, outcome: Outcome? = nil, errorKind: String? = nil) {
        self.response = response
        self.outcome = outcome
        self.errorKind = errorKind
    }

    enum CodingKeys: String, CodingKey {
        case response, outcome
        case errorKind = "error_kind"
    }

    var moduleFailureDescription: String? {
        guard outcome == .moduleError else { return nil }
        switch errorKind {
        case "cme": return "模块返回 +CME ERROR，请根据模块错误码检查设备或命令参数。"
        case "cms": return "模块返回 +CMS ERROR，请根据短信错误码检查短信服务或命令参数。"
        default: return "模块返回 ERROR；组合命令中较早的操作可能已执行。"
        }
    }
}

// MARK: - 网络

struct PDPContext: Decodable {
    let id: Int?
    let pdn: String?
    let apn: String?
}

struct MACInterface: Decodable {
    let name: String?
    let status: String?
    let ipv4: String?
    let kind: String?
}

struct DefaultRoute: Decodable {
    let interface: String?
    let gateway: String?
}

/// GET /api/network
struct NetworkDiagnostic: Decodable {
    let usbnetMode: String?
    let usbcfg: String?
    let pdpContexts: [PDPContext]?
    let activeContexts: [Int]?
    let pdpAddresses: [String]?
    let macInterfaces: [MACInterface]?
    let defaultRoute: DefaultRoute?
    let usbNetworkPresent: Bool?
    let errors: [String: String]?
    let partial: Bool?
    let capturedAtMS: Int64?
    let durationMS: Int64?
    let completedCommands: Int?

    enum CodingKeys: String, CodingKey {
        case usbcfg
        case usbnetMode = "usbnet_mode"
        case pdpContexts = "pdp_contexts"
        case activeContexts = "active_contexts"
        case pdpAddresses = "pdp_addresses"
        case macInterfaces = "mac_interfaces"
        case defaultRoute = "default_route"
        case usbNetworkPresent = "usb_network_present"
        case errors
        case partial
        case capturedAtMS = "captured_at_ms"
        case durationMS = "duration_ms"
        case completedCommands = "completed_commands"
    }
}

/// GET /api/network/traffic
struct TrafficSnapshot: Decodable {
    let available: Bool
    let interface: String?
    let rxBytes: UInt64?
    let txBytes: UInt64?
    let sessionRX: UInt64?
    let sessionTX: UInt64?
    let sessionTotal: UInt64?
    let sampledAtMS: Int64?
    let error: String?
    var recoveryRetryable: Bool? = nil

    enum CodingKeys: String, CodingKey {
        case available, interface, error
        case recoveryRetryable = "recovery_retryable"
        case rxBytes = "rx_bytes"
        case txBytes = "tx_bytes"
        case sessionRX = "session_rx_bytes"
        case sessionTX = "session_tx_bytes"
        case sessionTotal = "session_total_bytes"
        case sampledAtMS = "sampled_at_ms"
    }
}

/// Rust owns recovery admission, deadlines, cooldown and terminal outcome.
struct NetworkRecoveryOperation: Decodable {
    let operationID: String
    let phase: String
    let outcome: String?
    let reason: String?
    let pollAfterMS: Int
    enum CodingKeys: String, CodingKey {
        case phase, outcome, reason
        case operationID = "operation_id", pollAfterMS = "poll_after_ms"
    }
    var phaseMessage: String {
        switch phase {
        case "waiting": return "正在等待 4G 网卡恢复…"
        case "checking": return "正在检查 4G 网卡…"
        case "submitting", "rebooting": return "正在重启 4G 模块…"
        case "readback": return "正在等待 4G 网卡重新连接…"
        default: return "正在读取恢复结果…"
        }
    }
    var succeeded: Bool { outcome == "recovered" || outcome == "already_connected" }
    var resultMessage: String {
        switch outcome {
        case "recovered": return "4G 网卡已恢复"
        case "already_connected": return "4G 网卡当前已连接"
        case "cooldown": return "模块重启仍在安全冷却期，请稍后重试"
        case "not_recoverable": return "当前故障无法通过模块重启恢复"
        case "timed_out": return "30 次网络状态读回仍未恢复，请重试或拔插模块"
        case "unknown": return "模块重启已提交或结果未知；冷却期内不会重复执行"
        case "interrupted_before_submit": return "后台在提交模块重启前退出，本次未自动重放"
        case "stale_owner": return "模块连接代次已变化，本次未执行重启"
        default:
            switch reason {
            case "network_usbnet_not_enabled": return "请先开启 USB 网卡"
            case "radio_call_busy", "radio_call_state_unconfirmed", "module_radio_busy": return "模块或通话正在执行其他操作，请稍后重试"
            case "module_flight_mode_enabled": return "请先关闭飞行模式，再恢复网络"
            default: return "网络恢复未完成：\(reason ?? "未知状态")"
            }
        }
    }
}

/// POST /api/network/check-4g、check-proxy
struct CheckResult: Decodable {
    let ok: Bool
    let summary: String?
    let detail: String?
}

/// POST /api/network/usbnet
struct USBNetRequest: Encodable {
    let mode: Int
}

struct USBNetResult: Decodable {
    let mode: Int?
    let response: String?
    let needsReboot: Bool?

    enum CodingKeys: String, CodingKey {
        case mode, response
        case needsReboot = "needs_reboot"
    }
}

/// POST /api/network/reboot-module（202）
struct RebootResult: Decodable {
    let accepted: Bool
    let response: String?
}

/// GET/POST /api/module/flight-mode. `enabled` is only authoritative when the
/// backend has verified the module's RF mode, signal query, and registration.
struct ModuleFlightModeStatus: Decodable, Equatable {
    let available: Bool
    let enabled: Bool
    let verified: Bool
    let userConfigured: Bool
    let cfunMode: Int
    let owner: String
    let canToggle: Bool
    let registrationKnown: Bool
    let cellularRegistered: Bool?
    let ceregState: Int?
    let cregState: Int?
    let signalKnown: Bool
    let signalDetected: Bool?
    let signalLevel: Int?
    let summary: String
    let detail: String?

    enum CodingKeys: String, CodingKey {
        case available, enabled, verified, owner, summary, detail
        case userConfigured = "user_configured"
        case cfunMode = "cfun_mode"
        case canToggle = "can_toggle"
        case registrationKnown = "registration_known"
        case cellularRegistered = "cellular_registered"
        case ceregState = "cereg_state"
        case cregState = "creg_state"
        case signalKnown = "signal_known"
        case signalDetected = "signal_detected"
        case signalLevel = "signal_level"
    }
}

struct ModuleFlightModeRequest: Encodable {
    let enabled: Bool
    let userInitiated: Bool

    enum CodingKeys: String, CodingKey {
        case enabled
        case userInitiated = "user_initiated"
    }
}

struct NetworkService: Decodable, Identifiable {
    let name: String
    let device: String?
    let port: String?
    let usb: Bool?
    let module: Bool?

    var id: String { name }
}

/// PUT /api/network/services-order
struct ServicesOrderRequest: Encodable {
    let services: [String]
}
