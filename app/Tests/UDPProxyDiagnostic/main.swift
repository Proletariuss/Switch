import Foundation

@main
struct UDPProxyDiagnosticTests {
    static func main() throws {
        let decoder = JSONDecoder()
        let legacy = try decoder.decode(UDPProxyProbeResult.self, from: Data(#"{"ok":true,"udp_ready":true,"latency_ms":2}"#.utf8))
        precondition(!legacy.confirmedRoundtrip, "Legacy Associate success is not data reachability")
        let handshake = try decoder.decode(UDPProxyProbeResult.self, from: Data(#"{"ok":true,"tcp_connected":true,"authenticated":true,"udp_associate":true,"udp_roundtrip":false,"udp_ready":false,"scope":"proxy_handshake"}"#.utf8))
        precondition(!handshake.attemptedRoundtrip && !handshake.confirmedRoundtrip)
        precondition(handshake.roundtripEvidence == nil)
        precondition(handshake.summary.contains("未发送 UDP"))
        let dnsError = try decoder.decode(UDPProxyProbeResult.self, from: Data(#"{"ok":true,"tcp_connected":true,"authenticated":true,"udp_associate":true,"udp_roundtrip":true,"scope":"proxy_udp_dns","dns_response_code":3}"#.utf8))
        precondition(dnsError.confirmedRoundtrip && dnsError.summary.contains("未确认域名解析成功"))
        let dnsOK = try decoder.decode(UDPProxyProbeResult.self, from: Data(#"{"ok":true,"tcp_connected":true,"authenticated":true,"udp_associate":true,"udp_roundtrip":true,"scope":"proxy_udp_dns","dns_response_code":0}"#.utf8))
        precondition(dnsOK.confirmedRoundtrip && dnsOK.summary.contains("匹配"))
        for field in ["ok", "tcp_connected", "authenticated", "udp_associate"] {
            let data = Data("{\"ok\":true,\"tcp_connected\":true,\"authenticated\":true,\"udp_associate\":true,\"udp_roundtrip\":true,\"scope\":\"proxy_udp_dns\"}".replacingOccurrences(of: "\"\(field)\":true", with: "\"\(field)\":false").utf8)
            let contradictory = try decoder.decode(UDPProxyProbeResult.self, from: data)
            precondition(!contradictory.confirmedRoundtrip, "Contradictory phase evidence must not confirm UDP")
            precondition(contradictory.roundtripEvidence == false, "The visible UDP evidence card must not turn green on contradictory phases")
        }
        let originalInputs = UDPProxyDiagnosticInputs(proxy: "A:1080", dnsServer: "192.0.2.53:53", dnsName: "probe.invalid")
        let snapshot = UDPProxyDiagnosticResultSnapshot(inputs: originalInputs, value: dnsOK)
        precondition(snapshot.result(for: originalInputs)?.confirmedRoundtrip == true)
        for edited in [UDPProxyDiagnosticInputs(proxy: "B:1080", dnsServer: "192.0.2.53:53", dnsName: "probe.invalid"), UDPProxyDiagnosticInputs(proxy: "A:1080", dnsServer: "192.0.2.54:53", dnsName: "probe.invalid"), UDPProxyDiagnosticInputs(proxy: "A:1080", dnsServer: "192.0.2.53:53", dnsName: "other.invalid")] {
            precondition(snapshot.result(for: edited) == nil, "A previous success must disappear after editing its inputs")
        }
        let futureError = UDPProxyProbeResult.safeError("token=fixture-secret;192.0.2.53")
        precondition(!futureError.contains("fixture-secret") && !futureError.contains("192.0.2.53"))
        precondition(UDPProxyProbeResult.safeError("udp_roundtrip_timeout").contains("未收到"))
        let handshakeRequest = try JSONEncoder().encode(UDPProxyProbeRequest(proxy: "fixture:1080", udpDNS: nil))
        let handshakeJSON = try JSONSerialization.jsonObject(with: handshakeRequest) as! [String: Any]
        precondition(handshakeJSON["udp_dns"] == nil, "Handshake must not choose a default DNS destination")
        let explicitRequest = try JSONEncoder().encode(UDPProxyProbeRequest(proxy: "fixture:1080", udpDNS: .init(server: "192.0.2.53:53", name: "probe.invalid")))
        let explicitJSON = try JSONSerialization.jsonObject(with: explicitRequest) as! [String: Any]
        let destination = explicitJSON["udp_dns"] as! [String: Any]
        precondition(destination["server"] as? String == "192.0.2.53:53")
        precondition(destination["name"] as? String == "probe.invalid")
        print("PASS: UDP diagnostic legacy evidence, DNS scope/RCODE, safe errors and explicit destination")
    }
}
