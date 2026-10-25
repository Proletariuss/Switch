import Foundation

@main struct ModuleNetworkRecoveryTests {
    @MainActor static let api = ControlledAPI.shared

    @MainActor static func waitFor(_ count: Int) async {
        for _ in 0..<10_000 {
            if api.requests.count >= count { return }
            await Task.yield()
        }
        preconditionFailure("missing recovery HTTP request")
    }

    @MainActor static func settle() async {
        for _ in 0..<300 { await Task.yield() }
    }

    @MainActor static func body(_ index: Int) -> [String: String] {
        let data = api.requests[index].body!
        return try! JSONSerialization.jsonObject(with: data) as! [String: String]
    }

    @MainActor static func main() async throws {
        // Localized text no longer grants admission; the Rust field does.
        let decoder = JSONDecoder()
        let old = try decoder.decode(
            TrafficSnapshot.self,
            from: Data(#"{"available":false,"interface":"en8","error":"4G 模块网卡未连接"}"#.utf8))
        let admitted = try decoder.decode(
            TrafficSnapshot.self,
            from: Data(#"{"available":false,"error":"arbitrary translated text","recovery_retryable":true}"#.utf8))
        let backend = BackendProcess()
        let checkpoint = MemoryNetworkRecoveryCheckpoint()
        let store = ModuleNetworkStore(backend: backend, recoveryCheckpoint: checkpoint)
        store.updateTraffic(old)
        precondition(!store.canRetryConnection)
        store.updateTraffic(admitted)
        precondition(store.canRetryConnection)

        var firstToasts: [String] = []
        store.onToast = { message, _, _ in firstToasts.append(message) }
        store.recoverAfterWake()
        await waitFor(1)
        let firstBody = body(0)
        precondition(firstBody["trigger"] == "wake")
        let retainedID = firstBody["client_request_id"]!
        precondition(!retainedID.isEmpty)
        precondition(checkpoint.pending?.clientRequestID == retainedID)
        precondition(store.recovering)

        // Losing the initial POST response is not terminal: the fence and
        // exact request ID remain durable across a GUI Store restart.
        api.fail(0)
        await settle()
        precondition(store.recovering)
        precondition(checkpoint.pending?.clientRequestID == retainedID)
        precondition(firstToasts.isEmpty)
        store.reset()

        let restarted = ModuleNetworkStore(backend: backend, recoveryCheckpoint: checkpoint)
        var restartedToasts: [String] = []
        restarted.onToast = { message, _, _ in restartedToasts.append(message) }
        await waitFor(2)
        let resumedBody = body(1)
        precondition(resumedBody["client_request_id"] == retainedID)
        precondition(resumedBody["trigger"] == "wake")
        api.complete(
            1,
            #"{"operation_id":"stable-operation","phase":"finished","outcome":"unknown","reason":"daemon_restarted_after_reboot_arm","poll_after_ms":0}"#)
        await settle()
        precondition(!restarted.recovering)
        precondition(checkpoint.pending == nil)
        precondition(restartedToasts.isEmpty) // wake only announces recovery success

        // Explicit reboot uses the same request DTO/receipt owner while keeping
        // its historical route for API compatibility.
        restarted.reboot()
        await waitFor(3)
        precondition(api.requests[2].path == "api/network/reboot-module")
        let rebootBody = body(2)
        precondition(rebootBody["trigger"] == "reboot")
        precondition(rebootBody["client_request_id"]?.isEmpty == false)
        api.complete(
            2,
            #"{"operation_id":"direct-operation","phase":"finished","outcome":"recovered","reason":null,"poll_after_ms":0}"#)
        await settle()
        precondition(!restarted.recovering)
        precondition(checkpoint.pending == nil)
        precondition(restartedToasts == ["4G 网卡已恢复"])

        await durableCheckpointFaults()
        await usbnetStateTransitions()
        print("PASS: durable network request ID survives lost POST and GUI restart; direct reboot shares Rust receipt owner; production checkpoint synchronize/readback/corruption/clear faults fail closed")
    }

    @MainActor static func usbnetStateTransitions() async {
        let store = ModuleNetworkStore(backend: BackendProcess(),
                                       recoveryCheckpoint: MemoryNetworkRecoveryCheckpoint())
        // Let the initial diagnostics finish through the original rejection
        // fixture before explicitly controlling this operation's requests.
        await settle()
        api.usbnetTesting = true
        defer { api.usbnetTesting = false; store.reset() }
        var toasts: [String] = []
        store.onToast = { text, _, _ in toasts.append(text) }
        var index = api.requests.count
        store.loadUSBStatus()
        await waitFor(index + 1)
        let staleDiagnostic = index
        store.switchMode()
        await waitFor(index + 2)
        let write = index + 1
        precondition(api.requests[write].path == "api/network/usbnet")
        let request = try! JSONDecoder().decode(USBNetTestRequest.self, from: api.requests[write].body!)
        precondition(request.mode == 1, "derive enable from confirmed mode without a view-side toggle")
        precondition(store.busy && !store.usbnetEnabled)
        store.switchMode()
        store.loadUSBStatus()
        await settle()
        precondition(api.requests.count == index + 2, "busy must suppress duplicate writes and diagnostic races")
        api.complete(staleDiagnostic, #"{"usbnet_mode":"1"}"#)
        await settle()
        precondition(!store.usbnetEnabled, "cancelled old load cannot publish over an active action")
        api.complete(write, #"{"mode":1,"response":"OK","needs_reboot":true}"#)
        await waitFor(index + 3)
        precondition(!store.busy && store.needsReboot)
        precondition(store.actionMessage?.contains("重启") == true)
        precondition(!store.usbnetEnabled, "AT acknowledgement is not applied interface evidence")
        api.complete(index + 2, #"{"usbnet_mode":"1"}"#)
        await settle()
        precondition(store.usbnetEnabled)

        index = api.requests.count
        store.switchMode()
        await waitFor(index + 1)
        let disable = try! JSONDecoder().decode(USBNetTestRequest.self, from: api.requests[index].body!)
        precondition(disable.mode == 0 && store.usbnetEnabled)
        api.fail(index)
        await waitFor(index + 2)
        precondition(store.usbnetEnabled && store.actionError != nil)
        api.complete(index + 1, #"{"usbnet_mode":"1"}"#)
        await settle()
        precondition(store.usbnetEnabled, "failure preserves the last observed configuration")

        index = api.requests.count
        store.switchMode()
        await waitFor(index + 1)
        store.reset()
        let toastCount = toasts.count
        api.complete(index, #"{"mode":0,"needs_reboot":true}"#)
        await settle()
        precondition(!store.busy && !store.usbnetEnabled && !store.needsReboot)
        precondition(api.requests.count == index + 1 && toasts.count == toastCount,
                     "late acknowledgement after reset cannot refresh or announce success")
        print("PASS: USB mode has no optimistic toggle; duplicate and stale loads are fenced; saved mode needs reboot; failure and reset retain honest state")
    }

    private struct USBNetTestRequest: Decodable { let mode: Int }
    @MainActor static func durableCheckpointFaults() async {
        let key = "switch.network-recovery.pending.v1"
        for fault in ["synchronize", "discard", "readback"] {
            let suite = "switch.network.checkpoint.\(UUID().uuidString)"
            let defaults = FaultingNetworkDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            switch fault {
            case "synchronize": defaults.failSynchronization = true
            case "discard": defaults.discardWrites = true
            default: defaults.alterWriteBytes = true
            }
            let checkpoint = UserDefaultsNetworkRecoveryCheckpoint(defaults: defaults)
            let store = ModuleNetworkStore(backend: BackendProcess(), recoveryCheckpoint: checkpoint)
            let count = api.requests.count
            store.retryConnection()
            await settle()
            store.retryConnection()
            store.recoverAfterWake()
            await settle()
            precondition(api.requests.count == count, "failed save must submit zero POSTs: \(fault)")
            precondition(store.recovering && store.actionError != nil)
            precondition(store.recoveryMessage?.contains("持久化") == true)
            store.reset()
        }

        for stored: Any in [Data("{".utf8), Data(#"{"clientRequestID":"id","trigger":"future"}"#.utf8),
                            Data(#"{"clientRequestID":" ","trigger":"wake"}"#.utf8), "not data"] {
            let suite = "switch.network.corrupt.\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            defaults.set(stored, forKey: key)
            _ = defaults.synchronize()
            let count = api.requests.count
            for _ in 0..<2 { // A newly created GUI/checkpoint must retain the corrupt fence.
                let store = ModuleNetworkStore(backend: BackendProcess(),
                    recoveryCheckpoint: UserDefaultsNetworkRecoveryCheckpoint(defaults: defaults))
                await settle()
                store.retryConnection()
                store.recoverAfterWake()
                store.reboot()
                await settle()
                precondition(store.recovering && store.actionError != nil)
                precondition(api.requests.count == count, "corrupt checkpoint must never POST")
                precondition(defaults.object(forKey: key) != nil, "retain corrupt evidence")
                store.reset()
            }
        }

        for fault in ["synchronize", "readback"] {
            let suite = "switch.network.clear.\(UUID().uuidString)"
            let defaults = FaultingNetworkDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let checkpoint = UserDefaultsNetworkRecoveryCheckpoint(defaults: defaults)
            let store = ModuleNetworkStore(backend: BackendProcess(), recoveryCheckpoint: checkpoint)
            let index = api.requests.count
            store.retryConnection()
            await waitFor(index + 1)
            let retainedID = body(index)["client_request_id"]!
            if fault == "synchronize" { defaults.failSynchronization = true }
            else { defaults.discardRemoval = true }
            api.complete(index, #"{"operation_id":"clear-fault","phase":"finished","outcome":"recovered","poll_after_ms":0}"#)
            await settle()
            precondition(store.recovering && store.recoveryMessage?.contains("未能安全清除") == true)
            precondition(defaults.data(forKey: key) != nil, "failed clear restores request pointer")
            store.retryConnection()
            await settle()
            precondition(api.requests.count == index + 1, "failed clear must not submit another POST")
            store.reset()
            defaults.failSynchronization = false
            defaults.discardRemoval = false
            let restartedCheckpoint = UserDefaultsNetworkRecoveryCheckpoint(defaults: UserDefaults(suiteName: suite)!)
            let restarted = ModuleNetworkStore(backend: BackendProcess(), recoveryCheckpoint: restartedCheckpoint)
            await waitFor(index + 2)
            precondition(body(index + 1)["client_request_id"] == retainedID,
                         "GUI restart must resume original ID after failed clear")
            api.complete(index + 1, #"{"operation_id":"clear-fault","phase":"finished","outcome":"recovered","poll_after_ms":0}"#)
            await settle()
            precondition(!restarted.recovering && (try! restartedCheckpoint.load()) == nil,
                         "confirmed terminal clear releases durable fence")
            restarted.reset()
        }
    }

}
