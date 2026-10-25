import Combine
import Foundation

/// Only stable diagnostic codes cross this boundary; never store free-form
/// backend errors, archive paths, message contents or subscriber identities.
struct SMSStoreHealth: Codable, Equatable {
    enum State: String, Codable { case ready, unavailable }
    enum Reason: String, Codable {
        case corrupt = "store_corrupt"
        case ioFailed = "store_io_failed"
        case unsafePath = "store_path_unsafe"
        case limitExceeded = "store_limit_exceeded"
        case workerFailed = "store_worker_failed"

        var label: String {
            switch self {
            case .corrupt: return "记录校验未通过"
            case .ioFailed: return "存储读写未确认"
            case .unsafePath: return "存储路径或权限异常"
            case .limitExceeded: return "记录超出容量限制"
            case .workerFailed: return "存储服务执行中断"
            }
        }
    }
    struct Item: Codable, Equatable {
        let state: State
        let reason: Reason?
        let restartRequired: Bool
        enum CodingKeys: String, CodingKey {
            case state, reason
            case restartRequired = "restart_required"
        }
        var isConsistent: Bool {
            switch state {
            case .ready: return reason == nil && !restartRequired
            case .unavailable: return reason != nil && restartRequired
            }
        }
        var label: String { state == .ready ? "正常" : (reason?.label ?? "暂不可用") }
    }
    let schemaVersion: Int
    let degraded: Bool
    let deviceSend: Item
    let nwait: Item
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case degraded, nwait
        case deviceSend = "device_send"
    }
    init(from decoder: Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try fields.decode(Int.self, forKey: .schemaVersion)
        degraded = try fields.decode(Bool.self, forKey: .degraded)
        deviceSend = try fields.decode(Item.self, forKey: .deviceSend)
        nwait = try fields.decode(Item.self, forKey: .nwait)
        guard schemaVersion == 1, deviceSend.isConsistent, nwait.isConsistent,
              degraded == (deviceSend.state == .unavailable || nwait.state == .unavailable) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                debugDescription: "Inconsistent SMS storage health"))
        }
    }
}

struct SMSRuntimeHealthResponse: Decodable {
    // Optional for older Rust backends. Missing metadata never means healthy.
    let smsStoreHealth: SMSStoreHealth?
    enum CodingKeys: String, CodingKey { case smsStoreHealth = "sms_store_health" }
}

protocol SMSStoreHealthAPI {
    func read() async throws -> SMSRuntimeHealthResponse
}
private struct LiveSMSStoreHealthAPI: SMSStoreHealthAPI {
    func read() async throws -> SMSRuntimeHealthResponse {
        try await APIClient(timeoutInterval: 5).get("api/v2/runtime")
    }
}

@MainActor final class SMSStoreHealthStore: ObservableObject {
    @Published private(set) var snapshot: SMSStoreHealth?
    @Published private(set) var loading = false
    @Published private(set) var detail: String?
    private var revision: UInt64 = 0
    private let api: any SMSStoreHealthAPI

    init(api: (any SMSStoreHealthAPI)? = nil) { self.api = api ?? LiveSMSStoreHealthAPI() }
    func refresh() async {
        revision &+= 1
        let request = revision
        loading = true
        snapshot = nil
        detail = nil
        defer { if revision == request { loading = false } }
        do {
            let result = try await api.read()
            guard !Task.isCancelled, revision == request else { return }
            snapshot = result.smsStoreHealth
            if snapshot == nil { detail = "当前后端未提供短信存储健康信息。" }
        } catch {
            guard !Task.isCancelled, revision == request else { return }
            detail = "短信存储状态读取未完成，请重新查询。"
        }
    }
    func invalidate() {
        revision &+= 1
        snapshot = nil
        loading = false
        detail = nil
    }
}
