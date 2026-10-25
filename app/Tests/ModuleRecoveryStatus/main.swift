import Foundation

private enum FixtureError: Error { case unavailable }

private struct FixtureAPI: ModuleRecoveryStatusAPI {
    let response: ModuleRecoveryRuntimeResponse?
    let delayNanoseconds: UInt64

    func read() async throws -> ModuleRecoveryRuntimeResponse {
        if delayNanoseconds > 0 {
            try await Task.sleep(nanoseconds: delayNanoseconds)
        }
        guard let response else { throw FixtureError.unavailable }
        return response
    }
}

private func decode(_ json: String) throws -> ModuleRecoveryRuntimeResponse {
    try JSONDecoder().decode(ModuleRecoveryRuntimeResponse.self, from: Data(json.utf8))
}

@main private struct ModuleRecoveryStatusTests {
    @MainActor static func main() async throws {
        let quarantined = try decode("""
        {"module_recovery":{"state":"legacy_recovery_required",
          "reason_codes":["legacy_call_mode_lock","legacy_adb_interface_lock"],
          "historical_recovery_confirmed":false}}
        """)
        let status = try XCTUnwrap(quarantined.moduleRecovery)
        assert(status.isIsolated)
        assert(status.reasonSummary == "legacy_call_mode_lock · legacy_adb_interface_lock")

        let store = ModuleRecoveryStatusStore(api: FixtureAPI(response: quarantined, delayNanoseconds: 0))
        await store.refresh()
        assert(store.snapshot == status)

        let ordinary = try decode("{}")
        let ordinaryStore = ModuleRecoveryStatusStore(api: FixtureAPI(response: ordinary, delayNanoseconds: 0))
        await ordinaryStore.refresh()
        assert(ordinaryStore.snapshot == nil)

        let confirmed = try decode("""
        {"module_recovery":{"state":"legacy_recovery_required",
          "reason_codes":[],"historical_recovery_confirmed":true}}
        """)
        assert(confirmed.moduleRecovery?.isIsolated == false)

        let delayed = ModuleRecoveryStatusStore(api: FixtureAPI(response: quarantined, delayNanoseconds: 40_000_000))
        let read = Task { await delayed.refresh() }
        try await Task.sleep(nanoseconds: 2_000_000)
        delayed.invalidate()
        await read.value
        assert(delayed.snapshot == nil, "cancelled or replaced backend read revived stale quarantine")

        let unavailable = ModuleRecoveryStatusStore(api: FixtureAPI(response: nil, delayNanoseconds: 0))
        await unavailable.refresh()
        assert(unavailable.snapshot == nil)

        print("PASS module recovery runtime projection, exact reasons, normal state, and stale-read fence")
    }
}

private func XCTUnwrap<T>(_ value: T?) throws -> T {
    guard let value else { throw FixtureError.unavailable }
    return value
}
