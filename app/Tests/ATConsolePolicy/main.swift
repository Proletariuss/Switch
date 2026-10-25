import Foundation

@main
struct ATConsolePolicyTests {
    static func main() throws {
        let decoder = JSONDecoder()
        func result(_ text: String) throws -> ATResult {
            try decoder.decode(ATResult.self, from: Data(text.utf8))
        }
        let legacy = try result(#"{"response":"OK"}"#)
        precondition(legacy.response == "OK" && legacy.moduleFailureDescription == nil)
        let legacyInitializer = ATResult(response: "OK")
        precondition(legacyInitializer.moduleFailureDescription == nil)
        for kind in ["generic", "cme", "cms"] {
            let text = #"{"response":"ERROR","outcome":"module_error","error_kind":""# + kind + #""}"#
            let error = try result(text)
            precondition(error.response == "ERROR" && error.moduleFailureDescription != nil)
        }
        let complete = try result(#"{"response":"+VALUE: ERROR IN DATA\nOK","outcome":"success"}"#)
        precondition(complete.moduleFailureDescription == nil)
        do {
            _ = try result(#"{"response":"ERROR","outcome":"unexpected"}"#)
            preconditionFailure("Unknown outcomes must not be presented as success")
        } catch is DecodingError {}
        let multiline = ATRequest(command: "AT\nATI")
        let encoded = try JSONEncoder().encode(multiline)
        let fields = try JSONSerialization.jsonObject(with: encoded) as! [String: String]
        precondition(fields["command"] == "AT\nATI", "The client must not erase an invalid pasted newline")
        precondition(ATRequest(command: "AT+CFUN=1;+CMGD=1,4").impactDescriptions.count == 2)
        precondition(ATRequest(command: "ATD+447700900123;").impactDescriptions.count == 1)
        precondition(ATRequest(command: #"AT+QCFG="literal;+CMGD=1""#).impactDescriptions.isEmpty)
        precondition(ATRequest(command: "AT+CFUN?;+COPS=?;AT").impactDescriptions.isEmpty)
        precondition(ATRequest(command: "AT+CPMS=1;+CPMS=2").impactDescriptions.count == 1)
        print("AT console DTO: legacy compatibility, module errors, closed outcomes and input preservation passed")
    }
}
