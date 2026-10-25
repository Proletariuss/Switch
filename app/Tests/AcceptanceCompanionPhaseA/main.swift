import Foundation
import CryptoKit

@main enum PhaseAFixture {
    static func main() throws {
        var checks = 0
        func check(_ value: Bool, _ name: String) {
            precondition(value, name); checks += 1
        }
        func rejects(_ name: String, _ action: () throws -> Any) {
            do { _ = try action(); preconditionFailure("accepted: " + name) }
            catch { checks += 1 }
        }
        func raw(_ strings: [String]) -> [[UInt8]] { strings.map { Array($0.utf8) } }
        let option = "--switch-acceptance-capsule=/frozen/run/app.json"
        check(AppRawEntry.classify(raw(["App"])) == .production, "production empty")
        for args in [["--background"], ["--", "file"], ["--switch-acceptanc"], ["-switch-acceptanc"], ["x--switch-acceptance"], ["--other"]] {
            check(AppRawEntry.classify(raw(["App"] + args)) == .production, "unreserved production")
        }
        check(AppRawEntry.classify([[255], [254]]) == .production, "nonUTF8 historical production")
        check(AppRawEntry.classify(raw(["--switch-acceptance", "--background"])) == .production, "argv0 not selectable")
        guard case .candidate(let path) = AppRawEntry.classify(raw(["App", option])) else { fatalError("candidate") }
        check(path.value == "/frozen/run/app.json", "exact candidate")
        let invalid = [
            ["--switch-acceptance"], ["-switch-acceptance"], ["-switch-acceptance-capsule=/x/app.json"],
            ["--switch-acceptance-capsul"], ["--switch-acceptance-unknown"], ["--switch-acceptance-capsule="],
            [option, option], ["--background", option], [option, "--background"], ["positional", option],
            ["--", option], [option, "--"], [option, "tail"],
            ["-listen", "unix:/x/switch.sock", "-backend=at", option],
            ["--switch-acceptance-capsule=relative/app.json"], ["--switch-acceptance-capsule=/x/./app.json"],
            ["--switch-acceptance-capsule=/x/../app.json"], ["--switch-acceptance-capsule=/x//app.json"],
            ["--switch-acceptance-capsule=/x/app.json/"], ["--switch-acceptance-capsule=/x/daemon.json"],
            ["--switch-acceptance-capsule=/x/app.json\u{0000}"], [option + "suffix"]
        ]
        for args in invalid { check(AppRawEntry.classify(raw(["App"] + args)) == .reject(.invalidArguments), "invalid shape") }
        for args in [[[255], Array(option.utf8)], [Array("App".utf8), Array(option.utf8) + [255]],
                     [Array("App".utf8), [255], Array(option.utf8)], [Array("App".utf8), Array("-switch-acceptance".utf8) + [255]]] {
            check(AppRawEntry.classify(args) == .reject(.invalidArguments), "raw invalid UTF8")
        }
        check(try AppRawEntry.captureDarwinArguments().count > 0, "Darwin raw capture compiles and copies")

        func json(_ value: [String: Any]) throws -> Data { try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
        func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
        let source = String(repeating: "a", count: 64)
        let run = String(repeating: "b", count: 32)
        let inventoryObject: [String: Any] = ["schema_version": 1, "source_digest": source, "version": "3.51", "app_release_version": "3.51", "build_number": "351"]
        let bytes = try json(inventoryObject)
        let inventory = try AcceptanceBuildInventory.decodeFixture(bytes, anchor: digest(bytes))
        check(inventory.sourceDigest == source && inventory.buildNumber == "351", "inventory valid independent values")
        rejects("production anchor unavailable") { try AcceptanceBuildInventory.decode(bytes) }
        rejects("missing anchor") { try AcceptanceBuildInventory.decodeFixture(bytes, anchor: nil) }
        rejects("invalid anchor") { try AcceptanceBuildInventory.decodeFixture(bytes, anchor: String(repeating: "A", count: 64)) }
        rejects("digest mismatch") { try AcceptanceBuildInventory.decodeFixture(bytes, anchor: String(repeating: "0", count: 64)) }
        func inventoryReject(_ data: Data) { rejects("inventory strict") { try AcceptanceBuildInventory.decodeFixture(data, anchor: digest(data)) } }
        for key in inventoryObject.keys {
            var missing = inventoryObject; missing.removeValue(forKey: key); inventoryReject(try json(missing))
            for bad: Any in [NSNull(), true, 1.5, [], [:]] {
                var wrong = inventoryObject; wrong[key] = bad; inventoryReject(try json(wrong))
            }
        }
        for (key,bad): (String,Any) in [("schema_version", "1"), ("schema_version", 2), ("source_digest", source.uppercased()), ("version", ""), ("app_release_version", ""), ("build_number", ""), ("build_number", 351)] {
            var wrong = inventoryObject; wrong[key] = bad; inventoryReject(try json(wrong))
        }
        var unknown = inventoryObject; unknown["app_sha256"] = source; inventoryReject(try json(unknown))
        let inventoryText = String(decoding: bytes, as: UTF8.self)
        for text in ["{\"schema_version\":1," + inventoryText.dropFirst(),
                     "{\"schema_vers\\u0069on\":1," + inventoryText.dropFirst(), inventoryText + "{}",
                     inventoryText.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1.0"),
                     inventoryText.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1e0"),
                     inventoryText.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":01"),
                     inventoryText.replacingOccurrences(of: "\"version\":\"3.51\"", with: "\"version\":\"\\uD800\"") ] { inventoryReject(Data(text.utf8)) }
        inventoryReject(Data([255])); inventoryReject(Data())
        var boundary = bytes; boundary.append(Data(repeating: 32, count: 16384 - bytes.count))
        check(try AcceptanceBuildInventory.decodeFixture(boundary, anchor: digest(boundary)).buildNumber == "351", "16KiB accepted")
        boundary.append(32); inventoryReject(boundary)

        let expected = try AcceptanceDTOExpectation(runID: run, appPID: 10, daemonPID: 11, observedPeerPID: 11, inventory: inventory)
        rejects("wrong peer scalar") { try AcceptanceDTOExpectation(runID: run, appPID: 10, daemonPID: 11, observedPeerPID: 12, inventory: inventory) }
        rejects("missing app pid") { try AcceptanceDTOExpectation(runID: run, appPID: 0, daemonPID: 11, observedPeerPID: 11, inventory: inventory) }
        rejects("bad run expectation") { try AcceptanceDTOExpectation(runID: "unknown", appPID: 10, daemonPID: 11, observedPeerPID: 11, inventory: inventory) }
        var common: [String: Any] = ["schema_version": 1, "ok": true, "mode": "no-device-acceptance", "run_id": run, "daemon_pid": 11, "app_pid": 10, "hardware_owner": "disabled_no_device_acceptance", "hardware_ownership": false, "hardware_ready": false, "stores_opened": false, "providers_started": false, "external_network_enabled": false]
        var runtime = common
        runtime.merge(["backend": "rust", "source_digest": source, "version": "3.51", "app_release_version": "3.51", "build_number": "351"]) { _,new in new }
        common["service_ready"] = true
        let health = common
        check(runtime.count == 17 && health.count == 13, "exact cross-boundary key counts")
        check(try AcceptanceDTOParser.runtime(json(runtime), status: 200, expected: expected).runID == run, "runtime valid")
        check(try AcceptanceDTOParser.health(json(health), status: 200, expected: expected).daemonPID == 11, "health valid")
        for (object, isRuntime) in [(runtime, true), (health, false)] {
            func parse(_ data: Data, _ status: Int = 200) throws -> Any {
                if isRuntime { return try AcceptanceDTOParser.runtime(data, status: status, expected: expected) }
                return try AcceptanceDTOParser.health(data, status: status, expected: expected)
            }
            for key in object.keys {
                var missing = object; missing.removeValue(forKey: key)
                rejects("mandatory " + key) { try parse(json(missing)) }
                for bad: Any in [NSNull(), "false", 0, 1.5, [], [:]] {
                    var wrong = object; wrong[key] = bad
                    // Some strings already equal "false"? None in valid protocol.
                    rejects("type/value " + key) { try parse(json(wrong)) }
                }
                let text = String(decoding: try json(object), as: UTF8.self)
                let token = String(decoding: try json([key: object[key]!]), as: UTF8.self)
                rejects("duplicate " + key) { try parse(Data(("{" + token.dropFirst().dropLast() + "," + text.dropFirst()).utf8)) }
            }
            var extra = object; extra["cutover_ready"] = true; rejects("unknown") { try parse(json(extra)) }
            for (key, bad): (String, Any) in [("run_id",String(repeating:"c",count:32)), ("app_pid",11), ("daemon_pid",10), ("schema_version",true), ("mode","production"), ("hardware_owner","rust"), ("ok",false)] {
                var wrong = object; wrong[key] = bad; rejects("binding " + key) { try parse(json(wrong)) }
            }
            for key in ["hardware_ownership", "hardware_ready", "stores_opened", "providers_started", "external_network_enabled"] {
                var enabled = object; enabled[key] = true; rejects("negative capability") { try parse(json(enabled)) }
            }
            let text = String(decoding: try json(object), as: UTF8.self)
            for token in ["true", "1.0", "1e0", "01", "-1", "9223372036854775808"] {
                rejects("strict schema integer") { try parse(Data(text.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":" + token).utf8)) }
            }
            for suffix in ["{}", "x", "\u{0000}"] { rejects("trailing") { try parse(Data((text + suffix).utf8)) } }
            rejects("escaped key duplicate") { try parse(Data(("{\"schema_vers\\u0069on\":1," + text.dropFirst()).utf8)) }
            for status in [199,201,204,301,401,503] { rejects("exact status") { try parse(json(object), status) } }
            var padded = try json(object); padded.append(Data(repeating:32,count:16384-padded.count))
            _ = try parse(padded); checks += 1
            padded.append(32); rejects("oversize DTO") { try parse(padded) }
        }
        for key in ["backend", "source_digest", "version", "app_release_version", "build_number"] {
            var wrong = runtime; wrong[key] = "wrong"
            rejects("independent metadata " + key) { try AcceptanceDTOParser.runtime(json(wrong), status:200, expected:expected) }
        }
        rejects("health cannot initial ready") { try AcceptanceDTOParser.runtime(json(health), status:200, expected:expected) }
        rejects("runtime cannot heartbeat") { try AcceptanceDTOParser.health(json(runtime), status:200, expected:expected) }
        rejects("minimal production health") { try AcceptanceDTOParser.runtime(Data("{\"ok\":true}".utf8), status:200, expected:expected) }

        let id = AcceptanceEvidenceIdentity(runID:run,pid:11,generation:1)
        let stale = AcceptanceEvidenceIdentity(runID:run,pid:11,generation:2)
        let fault = StableIOFault(phase:.read,errno:5)
        let none = AcceptanceFaults([])
        let bad = AcceptanceCheckedStep.failed(fault)
        let fenceBinding = AcceptanceFenceBinding(capsuleSHA256:source,session:"session-1",reason:"requested-quit",timestampUnixMS:1)
        func pipe(_ kind: AcceptanceStreamKind, identity: AcceptanceEvidenceIdentity? = nil, end: AcceptanceStreamEnd = .eof,
                  worker: AcceptanceWorkerJoin = .joined, bytes: UInt64 = 3, delivered: UInt64 = 3, ack: UInt64 = 3,
                  write: AcceptanceCheckedStep = .succeeded, flush: AcceptanceDestinationFlush = .pipeDeliveryAcknowledged,
                  close: AcceptanceCheckedStep = .succeeded, reader: AcceptanceCheckedStep = .succeeded,
                  faults: AcceptanceFaults? = nil) -> AcceptancePipeOutcome {
            AcceptancePipeOutcome(identity:identity ?? id,stream:kind,end:end,worker:worker,bytesRead:bytes,bytesDelivered:delivered,logBytesAcknowledged:ack,
                destination:AcceptanceDestinationReport(writes:write,flush:flush,ownedClose:close),readerClose:reader,faults:faults ?? none)
        }
        func log(identity: AcceptanceEvidenceIdentity? = nil, step: Int? = nil, worker: AcceptanceWorkerJoin = .joined,
                 ack: UInt64 = 6, sequence: UInt64 = 2, last: UInt64 = 2, all: Bool = true) -> AcceptanceLogOutcome {
            let results = (0..<6).map { $0 == step ? bad : .succeeded }
            return AcceptanceLogOutcome(identity:identity ?? id,worker:worker,append:results[0],rotation:results[1],finalFlush:results[2],fileSync:results[3],ownedClose:results[4],directorySync:results[5],bytesAcknowledged:ack,finalSequence:sequence,lastAcknowledgedSequence:last,allSequencesAcknowledged:all,faults:none)
        }
        func fence(step: Int? = nil, binding: AcceptanceFenceBinding? = nil) -> AcceptanceFenceReport {
            let r = (0..<9).map { $0 == step ? bad : .succeeded }
            return AcceptanceFenceReport(identity:id,binding:binding ?? fenceBinding,write:r[0],readback:r[1],mode:r[2],fileSync:r[3],ownedClose:r[4],persistDirectorySync:r[5],clear:r[6],absence:r[7],clearDirectorySync:r[8])
        }
        func report(exit: AcceptanceChildExit = .requestedNormal(0), out: AcceptancePipeOutcome? = nil, err: AcceptancePipeOutcome? = nil,
                    writer: AcceptanceLogOutcome? = nil, quarantine: AcceptanceFenceReport? = nil,
                    proofIndex: Int? = nil, faults: AcceptanceFaults? = nil,
                    expectedBinding: AcceptanceFenceBinding? = nil) -> AcceptanceChildCleanupReport {
            let p = (0..<4).map { AcceptanceFinalProof(identity:id,result:$0 == proofIndex ? bad : .succeeded) }
            return AcceptanceChildCleanupReport(identity:id,exit:exit,expectedFenceBinding:expectedBinding ?? fenceBinding,stdout:out ?? pipe(.stdout),stderr:err ?? pipe(.stderr),writer:writer ?? log(),fence:quarantine ?? fence(),daemonRetirement:p[0],context:p[1],inventory:p[2],custody:p[3],earlierFaults:faults ?? none)
        }
        check(report().assessment == .evidenceComplete, "pure complete evidence summary")
        for exit: AcceptanceChildExit in [.requestedNormal(1),.unexpectedNormal(0),.signalled(15),.forced,.notObserved] { check(report(exit:exit).assessment == .failed,"exit cannot overwrite failure") }
        for end: AcceptanceStreamEnd in [.readFailure(fault),.cancelled,.deadline] {
            check(report(out:pipe(.stdout,end:end)).assessment == .failed,"stdout end failure")
            check(report(err:pipe(.stderr,end:end)).assessment == .failed,"stderr end failure")
        }
        for out in [pipe(.stdout,identity:stale),pipe(.stderr),pipe(.stdout,worker:.stillOwned),pipe(.stdout,delivered:2),pipe(.stdout,ack:2),pipe(.stdout,write:bad),pipe(.stdout,flush:.notPerformed),pipe(.stdout,flush:.fileFlush(bad)),pipe(.stdout,close:bad),pipe(.stdout,reader:bad),pipe(.stdout,faults:AcceptanceFaults([fault])),pipe(.stdout,faults:AcceptanceFaults([],alreadyOverflowed:1))] {
            check(report(out:out).assessment == .failed,"pipe independent faults")
        }
        for i in 0..<6 { check(report(writer:log(step:i)).assessment == .failed,"writer checked step") }
        for writer in [log(identity:stale),log(worker:.stillOwned),log(ack:5),log(last:1),log(all:false)] { check(report(writer:writer).assessment == .failed,"writer identity ack join") }
        for i in 0..<9 { check(report(quarantine:fence(step:i)).assessment == .failed,"fence checked step") }
        for binding in [AcceptanceFenceBinding(capsuleSHA256:String(repeating:"c",count:64),session:"session-1",reason:"requested-quit",timestampUnixMS:1),
                        AcceptanceFenceBinding(capsuleSHA256:source,session:"session-2",reason:"requested-quit",timestampUnixMS:1),
                        AcceptanceFenceBinding(capsuleSHA256:source,session:"",reason:"requested-quit",timestampUnixMS:1),
                        AcceptanceFenceBinding(capsuleSHA256:source,session:"session-1",reason:"",timestampUnixMS:1),
                        AcceptanceFenceBinding(capsuleSHA256:source,session:"session-1",reason:"requested-quit",timestampUnixMS:0)] {
            check(report(quarantine:fence(binding:binding)).assessment == .failed,"fence exact binding")
        }
        for i in 0..<4 { check(report(proofIndex:i).assessment == .failed,"mandatory final proof") }
        check(report(faults:AcceptanceFaults([fault])).assessment == .failed,"earlier fault retained after exit zero")
        let bounded = AcceptanceFaults(Array(repeating:fault,count:70))
        check(bounded.entries.count == 64 && bounded.overflowCount == 6 && !bounded.clean,"bounded overflow failure")
        check(AcceptanceFaults(Array(repeating:fault,count:70),alreadyOverflowed:UInt64.max).overflowCount == UInt64.max,"saturating overflow count")
        check(report(out:pipe(.stdout,bytes:UInt64.max,delivered:UInt64.max,ack:UInt64.max)).assessment == .failed,"aggregation integer overflow")
        // Keep the original 464 checks intact and count the regression additions separately.
        precondition(checks == 464, "original Phase A fixture check count drift")
        let originalChecks = checks
        let composed = "\u{00e9}", decomposed = "e\u{0301}"
        check(composed == decomposed && !composed.utf8.elementsEqual(decomposed.utf8), "regression distinguishes Swift canonical equality")
        for (exact, different, escaped) in [(composed, decomposed, "\\u00E9"), (decomposed, composed, "e\\u0301")] {
            for key in ["version", "app_release_version", "build_number"] {
                var input = inventoryObject; input[key] = exact
                let inputBytes = try json(input)
                let unicodeInventory = try AcceptanceBuildInventory.decodeFixture(inputBytes, anchor:digest(inputBytes))
                let unicodeExpected = try AcceptanceDTOExpectation(runID:run,appPID:10,daemonPID:11,observedPeerPID:11,inventory:unicodeInventory)
                var response = runtime; response[key] = exact
                let exactDTO = try AcceptanceDTOParser.runtime(json(response),status:200,expected:unicodeExpected)
                check(exactDTO == exactDTO && unicodeInventory == unicodeInventory, "exact Unicode inventory and DTO accepted " + key)
                response[key] = different
                rejects("scalar-distinct runtime " + key) { try AcceptanceDTOParser.runtime(json(response),status:200,expected:unicodeExpected) }
                response[key] = exact
                let token = "\"" + key + "\":\"" + exact + "\""
                let escapedToken = "\"" + key + "\":\"" + escaped + "\""
                let escapedResponse = String(decoding:try json(response),as:UTF8.self).replacingOccurrences(of:token,with:escapedToken)
                check(escapedResponse.contains(escapedToken), "escape spelling actually exercised " + key)
                check(try AcceptanceDTOParser.runtime(Data(escapedResponse.utf8),status:200,expected:unicodeExpected) == exactDTO, "same decoded scalar escape accepted " + key)
                let escapedInventoryBytes = Data(String(decoding:inputBytes,as:UTF8.self).replacingOccurrences(of:token,with:escapedToken).utf8)
                let escapedInventory = try AcceptanceBuildInventory.decodeFixture(escapedInventoryBytes,anchor:digest(escapedInventoryBytes))
                let actual = key == "version" ? escapedInventory.version : key == "app_release_version" ? escapedInventory.appReleaseVersion : escapedInventory.buildNumber
                check(actual.utf8.elementsEqual(exact.utf8), "inventory escape preserves exact decoded scalars " + key)
            }
            let exactBinding = AcceptanceFenceBinding(capsuleSHA256:source,session:exact,reason:exact,timestampUnixMS:1)
            check(report(quarantine:fence(binding:exactBinding),expectedBinding:exactBinding).assessment == .evidenceComplete, "exact Unicode fence allowed")
            for binding in [AcceptanceFenceBinding(capsuleSHA256:source,session:different,reason:exact,timestampUnixMS:1),
                            AcceptanceFenceBinding(capsuleSHA256:source,session:exact,reason:different,timestampUnixMS:1)] {
                check(binding.valid && binding != exactBinding, "scalar-distinct fence identity rejected")
                check(report(quarantine:fence(binding:binding),expectedBinding:exactBinding).assessment == .failed, "scalar-distinct fence cannot authorize cleanup")
            }
            let decoded = try JSONDecoder().decode(String.self,from:Data(("\"" + escaped + "\"").utf8))
            let escapedBinding = AcceptanceFenceBinding(capsuleSHA256:source,session:decoded,reason:decoded,timestampUnixMS:1)
            check(escapedBinding == exactBinding, "fence same decoded scalar escape accepted")
            check(report(quarantine:fence(binding:escapedBinding),expectedBinding:exactBinding).assessment == .evidenceComplete, "escaped fence cleanup exact")

            guard case .candidate(let exactPath) = AppRawEntry.classify(raw(["App","--switch-acceptance-capsule=/" + exact + "/app.json"])),
                  case .candidate(let otherPath) = AppRawEntry.classify(raw(["App","--switch-acceptance-capsule=/" + different + "/app.json"])) else { fatalError("Unicode paths must remain allowed") }
            check(exactPath != otherPath, "capsule path exact decoded scalar identity")
            check(AppRawEntry.candidate(exactPath) != .candidate(otherPath), "entry associated path exact identity")

            func dto(_ runValue: String = run, _ sourceValue: String = source, _ version: String = "3.51", _ release: String = "3.51", _ build: String = "351") -> AcceptanceRuntimeDTO {
                AcceptanceRuntimeDTO(runID:runValue,appPID:10,daemonPID:11,sourceDigest:sourceValue,version:version,appReleaseVersion:release,buildNumber:build)
            }
            check(dto(run,source,exact) != dto(run,source,different), "runtime DTO version equality exact")
            check(dto(run,source,"3.51",exact) != dto(run,source,"3.51",different), "runtime DTO release equality exact")
            check(dto(run,source,"3.51","3.51",exact) != dto(run,source,"3.51","3.51",different), "runtime DTO build equality exact")
            // These memberwise values are invalid protocol identities; even their equality stays exact.
            check(dto(exact) != dto(different), "runtime DTO run equality exact even before validation")
            check(dto(run,exact) != dto(run,different), "runtime DTO digest equality exact even before validation")
            check(AcceptanceHealthDTO(runID:exact,appPID:10,daemonPID:11) != AcceptanceHealthDTO(runID:different,appPID:10,daemonPID:11), "health DTO equality exact")
            check(AcceptanceEvidenceIdentity(runID:exact,pid:11,generation:1) != AcceptanceEvidenceIdentity(runID:different,pid:11,generation:1), "evidence identity equality exact")
        }
        // Valid scalar outside the BMP: literal and surrogate-pair JSON spellings agree.
        var supplementaryInput = inventoryObject; supplementaryInput["version"] = "\u{1f600}"
        let supplementaryBytes = try json(supplementaryInput)
        let supplementaryInventory = try AcceptanceBuildInventory.decodeFixture(supplementaryBytes,anchor:digest(supplementaryBytes))
        let supplementaryExpected = try AcceptanceDTOExpectation(runID:run,appPID:10,daemonPID:11,observedPeerPID:11,inventory:supplementaryInventory)
        var supplementaryRuntime = runtime; supplementaryRuntime["version"] = "\u{1f600}"
        let supplementaryText = String(decoding:try json(supplementaryRuntime),as:UTF8.self).replacingOccurrences(of:"\"version\":\"\u{1f600}\"",with:"\"version\":\"\\uD83D\\uDE00\"")
        check(supplementaryText.contains("\\uD83D\\uDE00"), "surrogate spelling actually exercised")
        check(try AcceptanceDTOParser.runtime(Data(supplementaryText.utf8),status:200,expected:supplementaryExpected).version.utf8.elementsEqual("\u{1f600}".utf8), "surrogate pair denotes same scalar")
        rejects("uppercase run expectation remains invalid") { try AcceptanceDTOExpectation(runID:run.uppercased(),appPID:10,daemonPID:11,observedPeerPID:11,inventory:inventory) }
        var uppercaseRuntime = runtime; uppercaseRuntime["source_digest"] = source.uppercased()
        rejects("uppercase source remains invalid") { try AcceptanceDTOParser.runtime(json(uppercaseRuntime),status:200,expected:expected) }
        check(!AcceptanceFenceBinding(capsuleSHA256:source.uppercased(),session:"s",reason:"r",timestampUnixMS:1).valid,"uppercase capsule hash remains invalid")
        let result: [String:Any] = ["schema_version":1,"decision":"PASS_PURE_PHASE_A_ONLY","checks":checks,"original_checks":originalChecks,"unicode_regression_checks":checks-originalChecks,"app_launched":false,"daemon_launched":false,"runtime_admission":"NO_GO","inventory_provenance":"NOT_IMPLEMENTED","actual_fd_peer_proof":"NOT_IMPLEMENTED","cleanup_io":"NOT_IMPLEMENTED"]
        print(String(decoding:try JSONSerialization.data(withJSONObject:result,options:[.prettyPrinted,.sortedKeys]),as:UTF8.self))
    }
}
