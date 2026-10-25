import Foundation

private struct CheckFailure: Error, CustomStringConvertible {
    let description: String
}

private func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CheckFailure(description: message) }
}

@main
private enum ReaderBLESelectionTests {
    static func main() throws {
        let usbReader = "USB:already-selected"
        let first = UUID(uuidString: "12345678-1234-1234-1234-123456789ABC")!
        let second = UUID(uuidString: "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE")!
        var selection = ReaderBLEManualSelection()
        var selectedReader = usbReader

        // Scan and remembered automatic BLE recovery may expose an APDU link,
        // but they are not a user request to replace the selected USB card.
        try expect(selection.connected(first, blockReason: nil) == .none,
                   "automatic BLE recovery selected a backend reader")
        try expect(selectedReader == usbReader, "automatic BLE recovery replaced USB")

        // Once GATT is already connected, the visible "Use" action must
        // explicitly select that reader without disconnecting and reconnecting.
        let useExisting = selection.useConnected(first, blockReason: nil)
        try expect(useExisting == .select("BLE:\(first.uuidString)"),
                   "explicit Use did not select an already-connected BLE reader")
        if case .select(let reader) = useExisting { selectedReader = reader }
        try expect(selectedReader == "BLE:\(first.uuidString)",
                   "Use selected the wrong BLE reader")
        selectedReader = usbReader
        try expect(selection.useConnected(first, blockReason: "VoWiFi 正在通话")
                    == .blocked("VoWiFi 正在通话"),
                   "Use bypassed the active VoWiFi switch guard")
        try expect(selectedReader == usbReader, "blocked Use replaced USB")

        selection.request(first)
        try expect(selection.connected(nil, blockReason: nil) == .none,
                   "old BLE teardown consumed the pending manual choice")
        let result = selection.connected(first, blockReason: nil)
        try expect(result == .select("BLE:\(first.uuidString)"),
                   "manual BLE connection did not select its backend reader")
        if case .select(let reader) = result { selectedReader = reader }
        try expect(selectedReader == "BLE:\(first.uuidString)", "selected reader differs from clicked BLE device")
        try expect(selection.connected(first, blockReason: nil) == .none,
                   "duplicate GATT completion selected the backend twice")

        selection.request(first)
        try expect(selection.connected(second, blockReason: nil) == .none,
                   "another BLE peripheral satisfied the manual request")
        try expect(selection.connected(first, blockReason: nil) == .none,
                   "stale connection stole the previous manual request")

        selection.request(first)
        let blocked = selection.connected(first, blockReason: "VoWiFi 正在通话")
        try expect(blocked == .blocked("VoWiFi 正在通话"),
                   "active VoWiFi did not prevent backend reader switch")
        try expect(selection.connected(first, blockReason: nil) == .none,
                   "blocked request was retried without a fresh user click")

        selectedReader = usbReader
        selection.request(first)
        selection.cancel() // User disconnect or sheet closure.
        try expect(selection.connected(nil, blockReason: nil) == .none,
                   "disconnect unexpectedly selected a BLE reader")
        try expect(selection.connected(first, blockReason: nil) == .none,
                   "late completion after disconnect selected a BLE reader")
        try expect(selectedReader == usbReader, "BLE disconnect affected the selected USB reader")

        var guardFixture = ReaderBLEGuardFixture()
        guardFixture.store.readerConnected = true
        guardFixture.store.readerName = usbReader
        guardFixture.store.readerVoWiFiLoaded = true
        guardFixture.store.readerVoWiFiStatus = .init(tunnelState: "disconnected", imsState: nil)
        try expect(guardFixture.bluetoothReaderSwitchBlockReason == nil,
                   "idle USB reader could not be switched")
        guardFixture.readerESIMStore.isBusy = true
        try expect(guardFixture.bluetoothReaderSwitchBlockReason != nil,
                   "active eSIM operation did not block reader switch")
        guardFixture.readerESIMStore.isBusy = false
        guardFixture.readerESIMStore.loadPhase = .loading
        try expect(guardFixture.bluetoothReaderSwitchBlockReason != nil,
                   "initial eSIM load did not block reader switch")
        guardFixture.readerESIMStore.loadPhase = .idle
        guardFixture.store.readerVoWiFiStatus = .init(tunnelState: "connected", imsState: "registered")
        try expect(guardFixture.bluetoothReaderSwitchBlockReason != nil,
                   "active USB VoWiFi did not block reader switch")
        guardFixture.store.readerVoWiFiStatus = .init(tunnelState: "disconnected", imsState: nil)
        guardFixture.store.readerName = "BLE:\(first.uuidString)"
        try expect(guardFixture.bluetoothReaderSwitchBlockReason != nil,
                   "selected BLE transport could be replaced before backend disconnect")

        print("Reader BLE manual selection: 13 focused cases passed")
    }
}
