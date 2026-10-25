import Foundation

/// Explicit proxy diagnostics. Destinations are supplied by the user and are
/// kept in view memory; these inputs are never included in shared summaries.
struct UDPProxyProbeRequest: Encodable {
    struct DNSDestination: Encodable {
        let server: String
        let name: String
    }

    let proxy: String
    let udpDNS: DNSDestination?

    enum CodingKeys: String, CodingKey {
        case proxy
        case udpDNS = "udp_dns"
    }
}

struct UDPProxyProbeResult: Decodable {
    let ok: Bool
    let tcpConnected: Bool?
    let authenticated: Bool?
    let udpAssociate: Bool?
    let udpRoundtrip: Bool?
    let scope: String?
    let stage: String?
    let latencyMS: UInt64?
    let roundtripLatencyMS: UInt64?
    let dnsResponseCode: UInt8?
    let errorCode: String?

    enum CodingKeys: String, CodingKey {
        case ok, authenticated, scope, stage
        case tcpConnected = "tcp_connected"
        case udpAssociate = "udp_associate"
        case udpRoundtrip = "udp_roundtrip"
        case latencyMS = "latency_ms"
        case roundtripLatencyMS = "roundtrip_latency_ms"
        case dnsResponseCode = "dns_response_code"
        case errorCode = "error_code"
    }

    var confirmedRoundtrip: Bool {
        scope == "proxy_udp_dns" && ok && tcpConnected == true && authenticated == true
            && udpAssociate == true && udpRoundtrip == true
    }
    var attemptedRoundtrip: Bool { scope == "proxy_udp_dns" }
    var roundtripEvidence: Bool? { attemptedRoundtrip ? confirmedRoundtrip : nil }

    var summary: String {
        if confirmedRoundtrip {
            if let code = dnsResponseCode, code != 0 {
                return "UDP 往返已确认；DNS 返回码 \(code)，未确认域名解析成功。"
            }
            return "已收到匹配的 DNS UDP 响应。"
        }
        if !attemptedRoundtrip, authenticated == true {
            return "代理握手已完成；本次未发送 UDP 探测。"
        }
        return Self.safeError(errorCode)
    }

    static func safeError(_ code: String?) -> String {
        switch code {
        case "invalid_proxy_address", "proxy_address_unavailable": return "代理地址无效或无法解析。"
        case "proxy_resolution_timeout": return "解析代理地址超时。"
        case "proxy_tcp_failed": return "无法建立代理 TCP 连接。"
        case "proxy_tcp_timeout": return "建立代理 TCP 连接超时。"
        case "proxy_auth_rejected": return "代理未接受无凭据 SOCKS5 握手。"
        case "proxy_auth_failed", "proxy_auth_timeout": return "代理握手失败或超时。"
        case "udp_associate_rejected": return "代理拒绝 UDP Associate。"
        case "udp_associate_failed", "udp_associate_timeout", "udp_associate_invalid": return "UDP Associate 未完成。"
        case "udp_relay_unavailable": return "代理未提供可用的 UDP relay 地址。"
        case "udp_roundtrip_timeout": return "Associate 已完成，但期限内未收到可验证的 UDP 响应。"
        case "proxy_control_closed": return "代理控制连接已关闭，UDP 关联失效。"
        case "udp_response_source_mismatch": return "UDP 响应来源与指定目的不符。"
        case "dns_response_identity_mismatch": return "DNS 响应标识或问题与请求不符。"
        case "dns_response_truncated": return "DNS 响应被截断，无法确认完整往返。"
        case "dns_response_invalid", "udp_response_invalid": return "响应格式无效，未确认 UDP 往返。"
        case "udp_send_failed", "udp_receive_failed", "udp_socket_failed": return "UDP 数据交换失败。"
        case "socks5_probe_timeout": return "代理握手检测超时。"
        case "socks5_handshake_failed": return "代理握手检测失败。"
        default: return "未确认 UDP 往返，请检查代理与探测目的配置。"
        }
    }
}

struct UDPProxyDiagnosticInputs: Equatable {
    let proxy: String
    let dnsServer: String
    let dnsName: String

    init(proxy: String, dnsServer: String, dnsName: String) {
        self.proxy = proxy.trimmingCharacters(in: .whitespacesAndNewlines)
        self.dnsServer = dnsServer.trimmingCharacters(in: .whitespacesAndNewlines)
        self.dnsName = dnsName.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// A completed result can only be shown beside the inputs it actually tested.
struct UDPProxyDiagnosticResultSnapshot {
    let inputs: UDPProxyDiagnosticInputs
    let value: UDPProxyProbeResult

    func result(for currentInputs: UDPProxyDiagnosticInputs) -> UDPProxyProbeResult? {
        inputs == currentInputs ? value : nil
    }
}
