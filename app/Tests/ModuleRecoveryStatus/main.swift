import AppKit
import Foundation
import SwiftUI

private enum FixtureError: Error { case unavailable }

/// Deliberately does not react to cancellation: late responses must be fenced
/// by the production store, not by a conveniently cooperative test transport.
private actor ControlledAPI: ModuleRecoveryStatusAPI {
    private var requests: [CheckedContinuation<ModuleRecoveryRuntimeResponse, Error>?] = []
    private var arrivals: [(Int, CheckedContinuation<Void, Never>)] = []

    func read() async throws -> ModuleRecoveryRuntimeResponse {
        try await withCheckedThrowingContinuation { continuation in
            requests.append(continuation)
            let ready = arrivals.filter { $0.0 <= requests.count }
            arrivals.removeAll { $0.0 <= requests.count }
            for (_, arrival) in ready { arrival.resume() }
        }
    }

    func waitForRequests(_ count: Int) async {
        if requests.count >= count { return }
        await withCheckedContinuation { arrivals.append((count, $0)) }
    }

    func finish(_ index: Int, _ result: Result<ModuleRecoveryRuntimeResponse, Error>) {
        precondition(requests.indices.contains(index))
        guard let continuation = requests[index] else { preconditionFailure("request already finished") }
        requests[index] = nil
        continuation.resume(with: result)
    }

    func requestCount() -> Int { requests.count }
}

private func decode(_ json: String) throws -> ModuleRecoveryRuntimeResponse {
    try JSONDecoder().decode(ModuleRecoveryRuntimeResponse.self, from: Data(json.utf8))
}

@main private struct ModuleRecoveryStatusTests {

    @MainActor static func mountedViewStartsRead(_ response: ModuleRecoveryRuntimeResponse, embedded: Bool) async throws {
        _ = NSApplication.shared
        let api = ControlledAPI()
        let store = ModuleRecoveryStatusStore(api: api)
        let backend = BackendProcess()
        let view = ModuleRecoveryStatusView(embedded: embedded, isolationReported: embedded, store: store)
            .environmentObject(backend)
        let host = NSHostingView(rootView: view)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 360),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        window.orderFront(nil)
        defer { window.orderOut(nil); window.close() }
        host.layoutSubtreeIfNeeded()
        for _ in 0..<200 {
            if await api.requestCount() > 0 { break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        let count = await api.requestCount()
        assert(count == 1, "mounted recovery card did not start exactly one read")
        assert(store.readState == .loading)
        await api.finish(0, .success(response))
        for _ in 0..<200 {
            if store.readState == .loaded { break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        assert(store.snapshot == response.moduleRecovery && store.readState == .loaded,
               "mounted recovery card did not consume its first response")
        backend.state = .stopped
        for _ in 0..<200 {
            if store.readState == .idle { break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        assert(store.snapshot == nil && store.readState == .idle,
               "mounted recovery card kept a stopped backend's observation")
    }
    @MainActor static func main() async throws {
        let quarantined = try decode("""
        {"module_recovery":{"state":"legacy_recovery_required",
          "reason_codes":["legacy_call_mode_lock","legacy_adb_interface_lock"],
          "historical_recovery_confirmed":false}}
        """)
        let status = try XCTUnwrap(quarantined.moduleRecovery)
        assert(status.isIsolated)
        assert(status.reasonSummary == "legacy_call_mode_lock · legacy_adb_interface_lock")
        let ordinary = try decode("{}")

        let reasons = [
            "legacy_execution_pending", "restore_journal_pending", "restore_journal_unreadable",
            "reconciliation_pending", "reconciliation_unreadable", "legacy_call_mode_lock",
            "legacy_adb_interface_lock", "legacy_lock_provenance_pending", "module_backup_present",
            "legacy_voice_runtime", "managed_voice_control_pending", "invalid_pristine_marker",
            "unreadable_evidence", "lifetime_authority_changed"
        ]
        for reason in reasons {
            let response = try decode("""
            {"module_recovery":{"state":"legacy_recovery_required",
              "reason_codes":["\(reason)","\(reason)"],"historical_recovery_confirmed":false}}
            """)
            assert(response.moduleRecovery?.reasonCodes == [reason])
        }
        for invalid in [
            #"{"state":"legacy_recovery_required","reason_codes":["private-path"],"historical_recovery_confirmed":false}"#,
            #"{"state":"future-state","reason_codes":["legacy_call_mode_lock"],"historical_recovery_confirmed":false}"#,
            #"{"state":"legacy_recovery_required","reason_codes":[],"historical_recovery_confirmed":false}"#,
            #"{"state":"legacy_recovery_required","reason_codes":["legacy_call_mode_lock"],"historical_recovery_confirmed":true}"#,
            #"{"state":"legacy_recovery_required","reason_codes":["legacy_call_mode_lock"],"historical_recovery_confirmed":0}"#
        ] {
            assert((try? decode(#"{"module_recovery":\#(invalid)}"#)) == nil,
                   "untrusted recovery state or reason was accepted for display")
        }

        let excessiveReasons: [String: Any] = ["module_recovery": [
            "state": "legacy_recovery_required",
            "reason_codes": Array(repeating: "legacy_call_mode_lock", count: 15),
            "historical_recovery_confirmed": false
        ]]
        let excessiveData = try JSONSerialization.data(withJSONObject: excessiveReasons)
        assert((try? JSONDecoder().decode(ModuleRecoveryRuntimeResponse.self, from: excessiveData)) == nil)

        let api = ControlledAPI()
        let store = ModuleRecoveryStatusStore(api: api)
        assert(store.snapshot == nil && store.readState == .idle)
        let first = Task { await store.refresh() }
        await api.waitForRequests(1)
        assert(store.snapshot == nil && store.readState == .loading)
        await api.finish(0, .success(quarantined))
        await first.value
        assert(store.snapshot == status && store.readState == .loaded)

        let failure = Task { await store.refresh() }
        await api.waitForRequests(2)
        assert(store.snapshot == nil && store.readState == .loading,
               "refresh presented the preceding observation as current")
        await api.finish(1, .failure(FixtureError.unavailable))
        await failure.value
        assert(store.snapshot == nil && store.readState == .failed,
               "read failure disappeared or implied successful recovery")

        let retry = Task { await store.refresh() }
        await api.waitForRequests(3)
        await api.finish(2, .success(ordinary))
        await retry.value
        assert(store.snapshot == nil && store.readState == .loaded,
               "successful non-isolated read retained an old read error")

        // An obsolete successful request must not replace a newer failure.
        let oldSuccess = Task { await store.refresh() }
        await api.waitForRequests(4)
        let currentFailure = Task { await store.refresh() }
        await api.waitForRequests(5)
        await api.finish(4, .failure(FixtureError.unavailable))
        await currentFailure.value
        await api.finish(3, .success(quarantined))
        await oldSuccess.value
        assert(store.snapshot == nil && store.readState == .failed)

        // An obsolete failure must not replace a newer successful observation.
        let oldFailure = Task { await store.refresh() }
        await api.waitForRequests(6)
        let currentSuccess = Task { await store.refresh() }
        await api.waitForRequests(7)
        await api.finish(6, .success(quarantined))
        await currentSuccess.value
        await api.finish(5, .failure(FixtureError.unavailable))
        await oldFailure.value
        assert(store.snapshot == status && store.readState == .loaded)

        // A backend change invalidates both successful and failed late reads.
        for result in [Result<ModuleRecoveryRuntimeResponse, Error>.success(quarantined),
                       .failure(FixtureError.unavailable)] {
            let index = await api.requestCount()
            let oldBackend = Task { await store.refresh() }
            await api.waitForRequests(index + 1)
            store.invalidate()
            assert(store.snapshot == nil && store.readState == .idle)
            await api.finish(index, result)
            await oldBackend.value
            assert(store.snapshot == nil && store.readState == .idle,
                   "an old backend response revived current UI state")
        }

        // Cancellation after submission cannot publish a late success.
        let index = await api.requestCount()
        let cancelled = Task { await store.refresh() }
        await api.waitForRequests(index + 1)
        cancelled.cancel()
        await api.finish(index, .success(quarantined))
        await cancelled.value
        assert(store.snapshot == nil && store.readState == .failed)
        store.invalidate()
        assert(store.readState == .idle)

        // A view task cancelled before it starts cannot invalidate its successor.
        let count = await api.requestCount()
        let beforeStart = Task { await store.refresh() }
        beforeStart.cancel()
        await beforeStart.value
        let after = await api.requestCount()
        assert(after == count && store.readState == .idle)

        try await mountedViewStartsRead(quarantined, embedded: false)
        try await mountedViewStartsRead(quarantined, embedded: true)

        print("PASS module recovery: visible read failures, retry, exact reasons, cancellation and stale backend/revision fences")
    }
}

private func XCTUnwrap<T>(_ value: T?) throws -> T {
    guard let value else { throw FixtureError.unavailable }
    return value
}
