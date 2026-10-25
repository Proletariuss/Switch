import Foundation

func iccidFlagEmoji(_ iccid: String) -> String? { nil }
func flagEmoji(countryCode: String) -> String { "" }

@main
private enum ReaderVoWiFiInputBoundaryTests {
    static func main() throws {
        let request = ReaderVoWiFiRequest(
            enable: true,
            proxy: nil,
            proxyHost: " 2001:db8::1 ",
            proxyPort: " 1080 ",
            epdg: " wlan.example.test ",
            epdgPortText: " 4500 ",
            observeOperation: true)
        let value = try JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as! [String: Any]
        precondition(value["proxy"] == nil, "Swift must not synthesize a proxy endpoint")
        precondition(value["proxy_host"] as? String == " 2001:db8::1 ")
        precondition(value["proxy_port"] as? String == " 1080 ")
        precondition(value["epdg"] as? String == " wlan.example.test ")
        precondition(value["epdg_port"] == nil, "Swift must not parse the business port")
        precondition(value["epdg_port_text"] as? String == " 4500 ")

        let invalid = ProxyCheckRequest(
            proxyHost: "socks5://bad host", proxyPort: "65536", epdg: "https://bad",
            sessionEPDG: " session.example ", readerEPDG: " reader.example ")
        let invalidValue = try JSONSerialization.jsonObject(with: JSONEncoder().encode(invalid)) as! [String: Any]
        precondition(invalidValue["proxy_host"] as? String == "socks5://bad host")
        precondition(invalidValue["proxy_port"] as? String == "65536")
        precondition(invalidValue["epdg"] as? String == "https://bad")
        precondition(invalidValue["session_epdg"] as? String == " session.example ")
        precondition(invalidValue["reader_epdg"] as? String == " reader.example ")
        print("PASS: Swift transports raw proxy/ePDG intent without validation or normalization")
    }
}
