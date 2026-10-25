import Foundation

// MARK: - 健康与 USB 设备

/// GET /api/health 的响应模型
struct HealthStatus: Decodable {
    let ok: Bool
    let port: String?
    let esimAvailable: Bool?
    let discoveryError: String?
    let usbDevice: USBDeviceStatus?

    enum CodingKeys: String, CodingKey {
        case ok, port
        case esimAvailable = "esim_available"
        case discoveryError = "discovery_error"
        case usbDevice = "usb_device"
    }
}

struct USBDeviceStatus: Decodable {
    let product: String?
    let vendor: String?
    let vendorID: String?
    let productID: String?
    let locationID: String?
    let speed: String?
    let mode: String?
    let interfaces: [USBInterface]?

    enum CodingKeys: String, CodingKey {
        case product, vendor, speed, mode, interfaces
        case vendorID = "vendor_id"
        case productID = "product_id"
        case locationID = "location_id"
    }
}

/// QMI session lifecycle reported by Rust. Initialization does not prove a
/// live link, SIM state, packet service, radio registration, or IMS readiness.
struct QmiDeviceStatus: Decodable {
    let enabled: Bool
    let sessionInitialized: Bool
    let initializationError: String?

    enum CodingKeys: String, CodingKey {
        case enabled
        case sessionInitialized = "session_initialized"
        case initializationError = "initialization_error"
    }

    init(enabled: Bool, sessionInitialized: Bool, initializationError: String? = nil) {
        self.enabled = enabled
        self.sessionInitialized = sessionInitialized
        self.initializationError = initializationError
    }
}

struct USBInterface: Decodable {
    let number: Int?
    let classID: Int?
    let subclass: Int?
    let protocolID: Int?
    let endpoints: Int?

    enum CodingKeys: String, CodingKey {
        case number, endpoints
        case classID = "class"
        case subclass, protocolID = "protocol"
    }
}

// MARK: - 模块状态

/// GET /api/status 的响应模型
struct DeviceStatus: Decodable {
    let imei: String?
    let firmware: String?
    let iccid: String?
    let imsi: String?
    let phoneNumber: String?
    let operatorName: String?
    let simInserted: Bool?
    let signalDbm: Int?
    let signalRsrp: Int?
    let signalRsrq: Int?
    let signalSinr: Int?
    let radioBand: String?
    let regStatus: Int?
    let regStatusText: String?
    let psAttached: Bool?
    let lac: String?
    let cellID: String?
    let apn: String?
    let imsStatus: Int?
    let networkMode: String?
    let networkDuplex: String?
    let usbnetMode: Int?
    let operatingMode: Int?
    let hardwareStatus: String?
    let discoveryError: String?
    let usbDevice: USBDeviceStatus?
    let qmi: QmiDeviceStatus?

    enum CodingKeys: String, CodingKey {
        case imei, firmware, iccid, imsi
        case phoneNumber = "phone_number"
        case operatorName = "operator"
        case simInserted = "sim_inserted"
        case signalDbm = "signal_dbm"
        case signalRsrp = "signal_rsrp"
        case signalRsrq = "signal_rsrq"
        case signalSinr = "signal_sinr"
        case radioBand = "radio_band"
        case regStatus = "reg_status"
        case regStatusText = "reg_status_text"
        case psAttached = "ps_attached"
        case lac
        case cellID = "cell_id"
        case apn, imsStatus = "ims_status"
        case networkMode = "network_mode"
        case networkDuplex = "network_duplex"
        case usbnetMode = "usbnet_mode"
        case operatingMode = "operating_mode"
        case hardwareStatus = "hardware_status"
        case discoveryError = "discovery_error"
        case usbDevice = "usb_device"
        case qmi
    }
}

/// Dashboard 对模块状态的唯一提交单元。
///
/// revision 是 App 内单调递增的提交序号；upstreamRevision 保存 Rust
/// 聚合快照的版本。所有相关字段一次替换，避免 SwiftUI 观察到 health、
/// status、来源和过期标记分属不同请求世代的中间状态。
struct DashboardModuleState {
    let revision: UInt64
    let upstreamRevision: UInt64?
    let health: HealthStatus?
    let status: DeviceStatus?
    let capturedAt: Date?
    let isStale: Bool
    let readPath: String
    let rustSnapshotComplete: Bool?
    let rustSnapshotIssues: [DeviceSnapshotIssue]
    let rustSnapshotError: String?

    static let initial = DashboardModuleState(
        revision: 0,
        upstreamRevision: nil,
        health: nil,
        status: nil,
        capturedAt: nil,
        isStale: false,
        readPath: "rust-native",
        rustSnapshotComplete: nil,
        rustSnapshotIssues: [],
        rustSnapshotError: nil)
}
