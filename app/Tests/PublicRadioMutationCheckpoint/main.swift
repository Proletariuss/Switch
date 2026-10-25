import Foundation

enum LocalBackendAuthentication { static let appToken="fixture"; static let headerName="X-Test" }
final class UnixSocketURLProtocol: URLProtocol { }
struct CheckFailure: Error { let message:String }
func require(_ value: @autoclosure () throws ->Bool,_ message:String)throws {if try !value(){throw CheckFailure(message:message)}}
final class FaultDefaults: UserDefaults {
    var memory:[String:Any]=[:]
    var fail=false
    override func object(forKey key:String)->Any? {memory[key]}
    override func set(_ value:Any?,forKey key:String) {memory[key]=value}
    override func synchronize()->Bool {!fail}
}
func request(_ path:String,_ body:String="{}") -> URLRequest {
    var r=URLRequest(url:URL(string:"http://fixture/"+path)!);r.httpMethod="POST";r.httpBody=Data(body.utf8);return r
}
func id(_ request:URLRequest)throws->String {let value=try JSONSerialization.jsonObject(with:request.httpBody!) as! [String:Any];return value["client_request_id"] as! String}
@main enum PublicRadioCheckpointTests {
    static func main() throws {
        let mode=CommandLine.arguments.dropFirst().first ?? "normal"
        if mode == "disk-write" || mode == "disk-read" {
            let suite=CommandLine.arguments[2]
            let expectedURL=URL(fileURLWithPath:CommandLine.arguments[3])
            let disk=UserDefaults(suiteName:suite)!
            PublicRadioMutationCheckpoint.defaults=disk
            if mode == "disk-write" {disk.removePersistentDomain(forName:suite);_ = disk.synchronize()}
            var command=request("api/at",#"{"command":"AT+UNKNOWNWRITE=1"}"#)
            _=try PublicRadioMutationCheckpoint.prepare(&command)
            if mode == "disk-write" {try command.httpBody!.write(to:expectedURL,options:.atomic)}
            else {
                try require(command.httpBody == Data(contentsOf:expectedURL),"new process changed persisted ID/body")
                disk.removePersistentDomain(forName:suite);try require(disk.synchronize(),"fixture defaults cleanup failed")
                print("PASS: separate GUI processes reload the exact persisted UserDefaults intent ID/body")
            }
            return
        }
        let defaults=FaultDefaults();PublicRadioMutationCheckpoint.defaults=defaults
        if mode=="corrupt" {
            defaults.memory["publicRadioMutationCheckpoint.v1"]="not Data"
            var a=request("api/at",#"{"command":"AT+CFUN=1,1"}"#)
            do {_=try PublicRadioMutationCheckpoint.prepare(&a);throw CheckFailure(message:"corruption admitted")}catch is APIOperationOutcomeUnknownError{}
            defaults.memory.removeAll()
            do {_=try PublicRadioMutationCheckpoint.prepare(&a);throw CheckFailure(message:"sticky corruption bypassed")}catch is APIOperationOutcomeUnknownError{}
            print("PASS: non-Data corruption remains sticky after replacement/reset");return
        }
        if mode=="save-failure" {
            defaults.fail=true
            var a=request("api/at",#"{"command":"AT+CFUN=1,1"}"#)
            do {_=try PublicRadioMutationCheckpoint.prepare(&a);throw CheckFailure(message:"failed save admitted")}catch is APIOperationOutcomeUnknownError{}
            defaults.fail=false
            do {_=try PublicRadioMutationCheckpoint.prepare(&a);throw CheckFailure(message:"failed save retry admitted")}catch is APIOperationOutcomeUnknownError{}
            print("PASS: failed persistence never admits a request or later in-memory retry");return
        }
        for path in ["api/at","api/module/flight-mode","api/network/usbnet","api/esim/module-notes","api/call-mode/enable","api/call-mode/restore","api/call/audio/start","api/sms/forwarding/queue-action","api/sms/clear","api/sms/delete-sender","api/calls/clear","api/v2/sms-archive/clear","api/v2/sms-archive/delete-sender"] {
            let payload=path=="api/at" ? #"{"command":"AT+CFUN=1,1"}"# : #"{"confirm":true}"#
            var first=request(path,payload);_=try PublicRadioMutationCheckpoint.prepare(&first)
            let original=first.httpBody;let originalID=try id(first)
            // A newly constructed request models GUI/APIClient replacement.
            var reopened=request(path,payload);_=try PublicRadioMutationCheckpoint.prepare(&reopened)
            try require(reopened.httpBody==original,"restart changed exact request \(path)")
            var drift=request(path,#"{"changed":true}"#)
            do {_=try PublicRadioMutationCheckpoint.prepare(&drift);throw CheckFailure(message:"drift admitted")}catch is APIOperationOutcomeUnknownError{}
            let receipt=try JSONSerialization.data(withJSONObject:["mutation_receipt":["client_request_id":originalID,"outcome":"completed"]])
            try PublicRadioMutationCheckpoint.completeResponse(path:path,data:receipt)
            var later=request(path,payload);_=try PublicRadioMutationCheckpoint.prepare(&later)
            try require(try id(later) != originalID,"completed operation reused old identity")
            let laterID=try id(later)
            try PublicRadioMutationCheckpoint.completeResponse(path:path,data:JSONSerialization.data(withJSONObject:["mutation_receipt":["client_request_id":laterID,"outcome":"completed"]]))
        }
        var query=request("api/at",#"{"command":"AT+CSQ"}"#)
        try require(try PublicRadioMutationCheckpoint.prepare(&query)==nil,"query required mutation receipt")
        var start=request("api/call/audio/start");_=try PublicRadioMutationCheckpoint.prepare(&start);let startID=try id(start)
        var stop=request("api/call/audio/stop");_=try PublicRadioMutationCheckpoint.prepare(&stop)
        let stopFields=try JSONSerialization.jsonObject(with:stop.httpBody!) as! [String:Any]
        try require(stopFields["audio_generation"] as? String==startID,"stop captured wrong generation")
        let stopID=try id(stop)
        let unknown=try JSONSerialization.data(withJSONObject:["mutation_receipt":["client_request_id":stopID,"outcome":"unknown"]])
        do {try PublicRadioMutationCheckpoint.completeResponse(path:"api/call/audio/stop",data:unknown);throw CheckFailure(message:"unknown cleared")}catch is APIOperationOutcomeUnknownError{}
        var retry=request("api/call/audio/stop");_=try PublicRadioMutationCheckpoint.prepare(&retry)
        try require(retry.httpBody==stop.httpBody,"unknown stop selected successor")
        let v2Unavailable=Data(#"{"code":"audio_identity_v2_unavailable","error":"audio_identity_v2_unavailable"}"#.utf8)
        try PublicRadioMutationCheckpoint.completeResponse(path:"api/call/audio/stop",data:v2Unavailable)
        var retained=request("api/call/audio/stop");_=try PublicRadioMutationCheckpoint.prepare(&retained)
        try require(retained.httpBody==stop.httpBody,"v2 unavailable response replaced v1 stop checkpoint")
        var retainedStart=request("api/call/audio/start");_=try PublicRadioMutationCheckpoint.prepare(&retainedStart)
        try require(try id(retainedStart)==startID,"v2 unavailable response replaced v1 start checkpoint")
        defaults.fail=true
        let complete=try JSONSerialization.data(withJSONObject:["mutation_receipt":["client_request_id":stopID,"outcome":"completed"]])
        do {try PublicRadioMutationCheckpoint.completeResponse(path:"api/call/audio/stop",data:complete);throw CheckFailure(message:"failed clear succeeded")}catch is APIOperationOutcomeUnknownError{}
        let stored=try JSONDecoder().decode(PublicRadioMutationCheckpoint.Archive.self,from:defaults.memory["publicRadioMutationCheckpoint.v1"] as! Data)
        try require(stored.pending["/api/call/audio/stop"]?.id==stopID,"failed clear lost original ID")
        print("PASS: 13 route checkpoint/restart/drift/receipt cases, audio exact generation, v2 rejection retention, unknown retention and failed-clear recovery")
    }
}
