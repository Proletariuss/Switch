import Combine
import CoreBluetooth
import Foundation
import Security

enum BluetoothReaderErrorCode: String {
    case unauthorized = "bluetooth_unauthorized"
    case poweredOff = "bluetooth_powered_off"
    case unsupported = "bluetooth_unsupported"
    case initializationTimeout = "bluetooth_initialization_timeout"
    case connectFailed = "bluetooth_connect_failed"
    case connectionTimeout = "bluetooth_connection_timeout"
    case serviceDiscoveryTimeout = "bluetooth_service_discovery_timeout"
    case characteristicDiscoveryTimeout = "bluetooth_characteristic_discovery_timeout"
    case notificationTimeout = "bluetooth_notification_timeout"
    case writeFailed = "bluetooth_write_failed"
    case serviceMissing = "estk_service_missing"
    case characteristicMissing = "estk_characteristic_missing"
    case notificationsFailed = "estk_notifications_failed"
    case disconnected = "bluetooth_disconnected"
    case apduBusy = "estk_apdu_busy"
    case apduTimeout = "estk_apdu_timeout"
    case apduMalformed = "estk_apdu_malformed"
    case resetUnsupported = "reader_reset_unsupported"
}

enum BluetoothReaderState: Equatable {
    case idle, scanning, connecting, discovering, connected, reconnecting, disconnected, failed
}

/// Reader protocol types.
/// - legacy: ESTKme-RED firmware with 3-byte framing
/// - ccid: ESTKme-RED with CCID (10-byte header)
/// - beeSim: 9esim / BeeSIM cards with [total, index] framing
/// - nineEsim: authenticated 9eSIM B1 over Nordic UART
private enum ESTKProtocol: Equatable { case legacy, ccid, beeSim, nineEsim }

private enum NineEsimCodecError: Error { case malformed, staleResponse }

@_silgen_name("switch_ble_detect_protocol")
private func switchBLEDetectProtocol(
    _ name: UnsafePointer<UInt8>?, _ nameLength: Int, _ hasNordicUART: UInt8
) -> UInt8

@_silgen_name("switch_ble_nble_device_suffix")
private func switchBLENBLEDeviceSuffix(
    _ name: UnsafePointer<UInt8>?, _ nameLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_nble_derive_key")
private func switchBLENBLEDeriveKey(
    _ name: UnsafePointer<UInt8>?, _ nameLength: Int,
    _ password: UnsafePointer<UInt8>?, _ passwordLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_nble_challenge_response")
private func switchBLENBLEChallengeResponse(
    _ challenge: UnsafePointer<UInt8>?, _ challengeLength: Int,
    _ key: UnsafePointer<UInt8>?, _ keyLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_nble_checksum")
private func switchBLENBLEChecksum(_ bytes: UnsafePointer<UInt8>?, _ length: Int) -> UInt16

@_silgen_name("switch_ble_nble_encode")
private func switchBLENBLEEncode(
    _ counter: UInt8, _ apdu: UnsafePointer<UInt8>?, _ apduLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_nble_decoder_create")
private func switchBLENBLEDecoderCreate() -> UnsafeMutableRawPointer?

@_silgen_name("switch_ble_nble_decoder_reset")
private func switchBLENBLEDecoderReset(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_nble_decoder_destroy")
private func switchBLENBLEDecoderDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_nble_decoder_add")
private func switchBLENBLEDecoderAdd(
    _ handle: UnsafeMutableRawPointer, _ frame: UnsafePointer<UInt8>?, _ frameLength: Int,
    _ expectedCounter: UInt8, _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_beesim_encode")
private func switchBLEBeeSIMEncode(
    _ data: UnsafePointer<UInt8>?, _ dataLength: Int, _ mtu: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_beesim_decoder_create")
private func switchBLEBeeSIMDecoderCreate() -> UnsafeMutableRawPointer?

@_silgen_name("switch_ble_beesim_decoder_destroy")
private func switchBLEBeeSIMDecoderDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_beesim_decoder_reset")
private func switchBLEBeeSIMDecoderReset(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_beesim_decoder_add")
private func switchBLEBeeSIMDecoderAdd(
    _ handle: UnsafeMutableRawPointer, _ frame: UnsafePointer<UInt8>?, _ frameLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_ccid_encode_apdu")
private func switchBLECCIDEncodeAPDU(
    _ apdu: UnsafePointer<UInt8>?, _ apduLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_ccid_extract_apdu")
private func switchBLECCIDExtractAPDU(
    _ frame: UnsafePointer<UInt8>?, _ frameLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_ccid_claim_continuation")
private func switchBLECCIDClaimContinuation(
    _ frame: UnsafePointer<UInt8>?, _ frameLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_legacy_encode_apdu")
private func switchBLELegacyEncodeAPDU(
    _ apdu: UnsafePointer<UInt8>?, _ apduLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_ccid_decoder_create")
private func switchBLECCIDDecoderCreate() -> UnsafeMutableRawPointer?

@_silgen_name("switch_ble_ccid_decoder_destroy")
private func switchBLECCIDDecoderDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_ccid_decoder_reset")
private func switchBLECCIDDecoderReset(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_ccid_decoder_add")
private func switchBLECCIDDecoderAdd(
    _ handle: UnsafeMutableRawPointer, _ data: UnsafePointer<UInt8>?, _ dataLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_legacy_decoder_create")
private func switchBLELegacyDecoderCreate() -> UnsafeMutableRawPointer?

@_silgen_name("switch_ble_legacy_decoder_destroy")
private func switchBLELegacyDecoderDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_legacy_decoder_reset")
private func switchBLELegacyDecoderReset(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_legacy_decoder_add")
private func switchBLELegacyDecoderAdd(
    _ handle: UnsafeMutableRawPointer, _ data: UnsafePointer<UInt8>?, _ dataLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_nble_handshake_create")
private func switchBLENBLEHandshakeCreate() -> UnsafeMutableRawPointer?

@_silgen_name("switch_ble_nble_handshake_destroy")
private func switchBLENBLEHandshakeDestroy(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_nble_handshake_reset")
private func switchBLENBLEHandshakeReset(_ handle: UnsafeMutableRawPointer)

@_silgen_name("switch_ble_nble_handshake_start")
private func switchBLENBLEHandshakeStart(_ handle: UnsafeMutableRawPointer) -> UInt8

@_silgen_name("switch_ble_nble_handshake_stage")
private func switchBLENBLEHandshakeStage(_ handle: UnsafeMutableRawPointer) -> UInt8

@_silgen_name("switch_ble_nble_handshake_next_counter")
private func switchBLENBLEHandshakeNextCounter(_ handle: UnsafeMutableRawPointer) -> UInt8

@_silgen_name("switch_ble_nble_handshake_expected_response_counter")
private func switchBLENBLEHandshakeExpectedResponseCounter(_ handle: UnsafeMutableRawPointer) -> UInt8

@_silgen_name("switch_ble_nble_handshake_challenge")
private func switchBLENBLEHandshakeChallenge(
    _ handle: UnsafeMutableRawPointer,
    _ challenge: UnsafePointer<UInt8>?, _ challengeLength: Int,
    _ key: UnsafePointer<UInt8>?, _ keyLength: Int,
    _ output: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int
) -> Int

@_silgen_name("switch_ble_nble_handshake_write_completed")
private func switchBLENBLEHandshakeWriteCompleted(_ handle: UnsafeMutableRawPointer) -> UInt8

@_silgen_name("switch_ble_nble_handshake_acknowledgement")
private func switchBLENBLEHandshakeAcknowledgement(
    _ handle: UnsafeMutableRawPointer, _ data: UnsafePointer<UInt8>?, _ dataLength: Int
) -> Int8

@_silgen_name("switch_ble_nble_keepalive_due")
private func switchBLENBLEKeepaliveDue(_ idleMilliseconds: UInt64, _ requestPending: UInt8) -> UInt8

// Rust owns retry admission, backoff, identity and explicit rejection policy.
@_silgen_name("switch_ble_reconnect_create")
private func switchBLEReconnectCreate() -> UnsafeMutableRawPointer?
@_silgen_name("switch_ble_reconnect_destroy")
private func switchBLEReconnectDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_ble_reconnect_event")
private func switchBLEReconnectEvent(_ handle: UnsafeMutableRawPointer, _ epoch: UInt64, _ event: UInt8) -> UInt64
@_silgen_name("switch_ble_reconnect_schedule")
private func switchBLEReconnectSchedule(_ handle: UnsafeMutableRawPointer, _ epoch: UInt64) -> UInt64
@_silgen_name("switch_ble_reconnect_delay")
private func switchBLEReconnectDelay(_ handle: UnsafeMutableRawPointer, _ ticket: UInt64) -> UInt32
@_silgen_name("switch_ble_reconnect_fire")
private func switchBLEReconnectFire(_ handle: UnsafeMutableRawPointer, _ epoch: UInt64, _ ticket: UInt64) -> UInt8
@_silgen_name("switch_ble_reconnect_stopped")
private func switchBLEReconnectStopped(_ handle: UnsafeMutableRawPointer) -> UInt8

private final class BLEReconnectPolicy {
    private let handle: UnsafeMutableRawPointer
    // Production callbacks and bridge requests use .main, but the manager is
    // not actor-isolated. Fence every Rust mutable borrow, including epoch.
    private let lock = NSLock()
    private var epoch: UInt64 = 0
    init() {
        guard let handle = switchBLEReconnectCreate() else { preconditionFailure("Rust BLE retry policy unavailable") }
        self.handle = handle
    }
    deinit { switchBLEReconnectDestroy(handle) }
    private func locked<T>(_ body: () -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }
    var stopped: Bool { locked { switchBLEReconnectStopped(handle) != 0 } }
    func select() { locked { epoch = switchBLEReconnectEvent(handle, epoch, 1) } }
    func stop() { locked { _ = switchBLEReconnectEvent(handle, epoch, 2) } }
    func rejectAuthentication() -> Bool { locked { switchBLEReconnectEvent(handle, epoch, 3) == 1 } }
    func ready() { locked { _ = switchBLEReconnectEvent(handle, epoch, 4) } }
    func cancelPending() { locked { _ = switchBLEReconnectEvent(handle, epoch, 5) } }
    func schedule() -> (epoch: UInt64, ticket: UInt64, delay: Double)? {
        locked {
            let ticket = switchBLEReconnectSchedule(handle, epoch)
            guard ticket != 0 else { return nil }
            return (epoch, ticket, Double(switchBLEReconnectDelay(handle, ticket)))
        }
    }
    func fire(epoch: UInt64, ticket: UInt64) -> Bool {
        locked { switchBLEReconnectFire(handle, epoch, ticket) == 1 }
    }
}

@_silgen_name("switch_ble_card_workflow_create")
private func switchBLECardWorkflowCreate(_ proto: UInt8, _ operation: UInt8) -> UnsafeMutableRawPointer?
@_silgen_name("switch_ble_card_workflow_destroy")
private func switchBLECardWorkflowDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_ble_card_workflow_status")
private func switchBLECardWorkflowStatus(_ handle: UnsafeMutableRawPointer) -> Int32
@_silgen_name("switch_ble_card_workflow_step")
private func switchBLECardWorkflowStep(_ handle: UnsafeMutableRawPointer) -> UInt64
@_silgen_name("switch_ble_card_workflow_command")
private func switchBLECardWorkflowCommand(_ handle: UnsafeMutableRawPointer, _ output: UnsafeMutablePointer<UInt8>?, _ capacity: Int) -> Int
@_silgen_name("switch_ble_card_workflow_advance")
private func switchBLECardWorkflowAdvance(_ handle: UnsafeMutableRawPointer, _ step: UInt64, _ success: UInt8, _ response: UnsafePointer<UInt8>?, _ length: Int) -> UInt8

@_silgen_name("switch_ble_gatt_selection_create")
private func switchBLEGATTCreate(_ proto: UInt8, _ services: Int) -> UnsafeMutableRawPointer?
@_silgen_name("switch_ble_gatt_selection_destroy")
private func switchBLEGATTDestroy(_ handle: UnsafeMutableRawPointer)
@_silgen_name("switch_ble_gatt_selection_begin")
private func switchBLEGATTBegin(_ handle: UnsafeMutableRawPointer, _ serviceMatches: UInt8, _ alreadyBound: UInt8)
@_silgen_name("switch_ble_gatt_selection_add")
private func switchBLEGATTAdd(_ handle: UnsafeMutableRawPointer, _ id: UInt32,
    _ uuid: UnsafePointer<UInt8>?, _ length: Int, _ properties: UInt8, _ matches: UInt8) -> UInt8
@_silgen_name("switch_ble_gatt_selection_finish")
private func switchBLEGATTFinish(_ handle: UnsafeMutableRawPointer) -> Int32
@_silgen_name("switch_ble_gatt_selection_updates")
private func switchBLEGATTUpdates(_ handle: UnsafeMutableRawPointer) -> UInt8
@_silgen_name("switch_ble_gatt_selection_pair")
private func switchBLEGATTPair(_ handle: UnsafeMutableRawPointer) -> UInt64
@_silgen_name("switch_ble_gatt_selection_remaining")
private func switchBLEGATTRemaining(_ handle: UnsafeMutableRawPointer) -> Int

/// Retains CoreBluetooth objects only. Ordered hints, fallbacks, discovery
/// counts and ready/failure decisions live behind the Rust handle.
private final class BLEGATTSelection {
    private let handle: UnsafeMutableRawPointer
    private let lock = NSLock()
    private var objects: [CBCharacteristic] = []
    private let estkService = CBUUID(string: "4553")
    private let nusService = CBUUID(string: "6e400001-b5a3-f393-e0a9-e50e24dcca9e")
    private let exactUUIDs = [CBUUID(string: "6D65"), CBUUID(string: "544B"),
        CBUUID(string: "6e400002-b5a3-f393-e0a9-e50e24dcca9e"),
        CBUUID(string: "6e400003-b5a3-f393-e0a9-e50e24dcca9e")]
    init(proto: ESTKProtocol, services: Int) {
        let code: UInt8
        switch proto { case .legacy: code = 0; case .ccid: code = 1; case .beeSim: code = 2; case .nineEsim: code = 3 }
        guard let handle = switchBLEGATTCreate(code, services) else {
            preconditionFailure("Rust GATT selection unavailable")
        }
        self.handle = handle
    }
    deinit { switchBLEGATTDestroy(handle) }
    var remaining: Int {
        lock.lock(); defer { lock.unlock() }
        return switchBLEGATTRemaining(handle)
    }
    func observe(_ service: CBService, alreadyBound: Bool) -> (status: Int32, updates: Bool, writer: CBCharacteristic?, notifier: CBCharacteristic?) {
        lock.lock(); defer { lock.unlock() }
        // Equality is a CoreBluetooth observation, preserving its short/base
        // UUID aliases. No Swift branch chooses a candidate or fallback.
        let serviceMatches: UInt8 = (service.uuid == estkService ? 1 : 0) | (service.uuid == nusService ? 2 : 0)
        switchBLEGATTBegin(handle, serviceMatches, alreadyBound ? 1 : 0)
        for characteristic in service.characteristics ?? [] {
            guard let id = UInt32(exactly: objects.count + 1) else { return (-127, false, nil, nil) }
            objects.append(characteristic)
            let p = characteristic.properties
            let properties: UInt8 = (p.contains(.write) ? 1 : 0) | (p.contains(.writeWithoutResponse) ? 2 : 0)
                | (p.contains(.notify) ? 4 : 0) | (p.contains(.indicate) ? 8 : 0)
            var matches: UInt8 = 0
            for (index, uuid) in exactUUIDs.enumerated() {
                matches |= characteristic.uuid == uuid ? UInt8(1 << index) : 0
            }
            let accepted = withBLEInput(Data(characteristic.uuid.uuidString.utf8)) { bytes, length in
                switchBLEGATTAdd(handle, id, bytes, length, properties, matches)
            }
            guard accepted == 1 else { return (-127, false, nil, nil) }
        }
        let status = switchBLEGATTFinish(handle)
        let pair = switchBLEGATTPair(handle)
        let writer = Int(pair >> 32), notifier = Int(UInt32(truncatingIfNeeded: pair))
        return (status, switchBLEGATTUpdates(handle) != 0,
            writer > 0 && writer <= objects.count ? objects[writer - 1] : nil,
            notifier > 0 && notifier <= objects.count ? objects[notifier - 1] : nil)
    }
}

private final class BLECardWorkflow {
    private let handle: UnsafeMutableRawPointer
    private let lock = NSLock()
    init?(proto: ESTKProtocol, reset: Bool = false) {
        let code: UInt8
        switch proto { case .legacy: code = 0; case .ccid: code = 1; case .beeSim: code = 2; case .nineEsim: code = 3 }
        guard let handle = switchBLECardWorkflowCreate(code, reset ? 2 : 1) else { return nil }
        self.handle = handle
    }
    deinit { switchBLECardWorkflowDestroy(handle) }
    func action() -> (status: Int32, step: UInt64, command: Data) {
        lock.lock()
        defer { lock.unlock() }
        let status = switchBLECardWorkflowStatus(handle)
        guard status == 1 || status == 2 || status == 5 || status == 6 else { return (status, 0, Data()) }
        let (command, count) = withBLEOutput(32) { output, capacity in
            switchBLECardWorkflowCommand(handle, output, capacity)
        }
        guard count > 0, let command else { return (-17, 0, Data()) }
        return (status, switchBLECardWorkflowStep(handle), command)
    }
    func advance(step: UInt64, result: Result<Data, Error>) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let success: UInt8
        let response: Data
        switch result {
        case .success(let data): success = 1; response = data
        case .failure: success = 0; response = Data()
        }
        return withBLEInput(response) { bytes, count in
            switchBLECardWorkflowAdvance(handle, step, success, bytes, count) == 1
        }
    }
}

private func withBLEInput<R>(_ data: Data, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    try data.withUnsafeBytes { bytes in
        try body(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count)
    }
}

private func withBLEOutput(_ capacity: Int, _ body: (UnsafeMutablePointer<UInt8>?, Int) -> Int) -> (Data?, Int) {
    var output = Data(count: capacity)
    let count = output.withUnsafeMutableBytes { bytes in
        body(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count)
    }
    guard count >= 0, count <= output.count else { return (nil, count) }
    output.count = count
    return (output, count)
}

private enum NineEsimHandshakeStage: Int {
    case waitingChallenge = 1
    case writingChallengeResponse = 2
    case waitingAuthenticationAck = 3
}

private final class NineEsimHandshake {
    private let handle: UnsafeMutableRawPointer

    init() {
        guard let handle = switchBLENBLEHandshakeCreate() else {
            preconditionFailure("Rust NBLE handshake allocation failed")
        }
        self.handle = handle
    }

    deinit { switchBLENBLEHandshakeDestroy(handle) }

    var stage: NineEsimHandshakeStage? {
        NineEsimHandshakeStage(rawValue: Int(switchBLENBLEHandshakeStage(handle)))
    }

    func reset() { switchBLENBLEHandshakeReset(handle) }

    func start() { _ = switchBLENBLEHandshakeStart(handle) }

    func nextCounter() -> UInt8 { switchBLENBLEHandshakeNextCounter(handle) }

    var expectedResponseCounter: UInt8 {
        switchBLENBLEHandshakeExpectedResponseCounter(handle)
    }

    func respond(to challenge: Data, key: Data) throws -> Data {
        let (response, status) = withBLEOutput(17) { output, capacity in
            withBLEInput(challenge) { challengeBytes, challengeCount in
                withBLEInput(key) { keyBytes, keyCount in
                    switchBLENBLEHandshakeChallenge(
                        handle, challengeBytes, challengeCount, keyBytes, keyCount, output, capacity)
                }
            }
        }
        guard status == 17, let response else { throw NineEsimCodecError.malformed }
        return response
    }

    func responseWriteCompleted() -> Bool {
        switchBLENBLEHandshakeWriteCompleted(handle) == 1
    }

    func classifyAcknowledgement(_ data: Data) -> Int8 {
        withBLEInput(data) { bytes, count in
            switchBLENBLEHandshakeAcknowledgement(handle, bytes, count)
        }
    }
}

/// Stores optional per-reader 9eSIM Bluetooth passwords in the local Keychain.
/// The B0 download PSK uses the same service with a separate account prefix.
enum NineEsimBluetoothCredentials {
    private static let service = "clarivate.nine-esim"

    static func account(for id: UUID) -> String {
        "ble-password:\(id.uuidString.lowercased())"
    }

    static func password(for id: UUID) throws -> String? {
        guard !AcceptanceBootstrap.isAcceptance else {
            throw NSError(domain: "SwitchAcceptanceUnavailable", code: 1)
        }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account(for: id),
            kSecAttrSynchronizable as String: false,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess,
              let data = result as? Data,
              let value = String(data: data, encoding: .utf8),
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
        return value
    }

    static func save(_ password: String?, for id: UUID) throws {
        guard !AcceptanceBootstrap.isAcceptance else {
            throw NSError(domain: "SwitchAcceptanceUnavailable", code: 1)
        }
        let identity: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account(for: id),
            kSecAttrSynchronizable as String: false
        ]
        let value = password?.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value, !value.isEmpty else {
            let status = SecItemDelete(identity as CFDictionary)
            guard status == errSecSuccess || status == errSecItemNotFound else {
                throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
            }
            return
        }
        guard value.count <= 128 else {
            throw NSError(domain: "NineEsimBluetoothCredentials", code: 1)
        }
        let attributes: [String: Any] = [
            kSecValueData as String: Data(value.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        let status = SecItemUpdate(identity as CFDictionary, attributes as CFDictionary)
        let finalStatus: OSStatus
        if status == errSecItemNotFound {
            var insertion = identity
            attributes.forEach { insertion[$0.key] = $0.value }
            finalStatus = SecItemAdd(insertion as CFDictionary, nil)
        } else {
            finalStatus = status
        }
        guard finalStatus == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(finalStatus))
        }
    }
}

/// The B1 NBLE protocol is independent from the older AE30/BeeSIM framing.
/// Credentials are derived in memory; this type never logs them or any APDU.
private enum NineEsimProtocol {
    static func defaultPassword(deviceName: String) throws -> String {
        let suffix = try deviceSuffix(deviceName)
        return String(suffix.reversed())
    }

    static func resolvedPassword(deviceName: String, storedPassword: String?) throws -> String {
        if let storedPassword {
            let value = storedPassword.trimmingCharacters(in: .whitespacesAndNewlines)
            if !value.isEmpty { return value }
        }
        return try defaultPassword(deviceName: deviceName)
    }

    static func deviceSuffix(_ deviceName: String) throws -> String {
        let name = Data(deviceName.utf8)
        let (result, status) = withBLEOutput(6) { output, capacity in
            withBLEInput(name) { bytes, count in
                switchBLENBLEDeviceSuffix(bytes, count, output, capacity)
            }
        }
        guard status == 6, let result, let suffix = String(data: result, encoding: .utf8) else {
            throw NineEsimCodecError.malformed
        }
        return suffix
    }

    static func deriveKey(deviceName: String, password: String) throws -> Data {
        let name = Data(deviceName.utf8)
        let password = Data(password.utf8)
        let (key, status) = withBLEOutput(16) { output, capacity in
            withBLEInput(name) { nameBytes, nameCount in
                withBLEInput(password) { passwordBytes, passwordCount in
                    switchBLENBLEDeriveKey(nameBytes, nameCount, passwordBytes, passwordCount, output, capacity)
                }
            }
        }
        guard status == 16, let key else { throw NineEsimCodecError.malformed }
        return key
    }

    static func challengeResponse(_ challenge: Data, key: Data) throws -> Data {
        let (response, status) = withBLEOutput(17) { output, capacity in
            withBLEInput(challenge) { challengeBytes, challengeCount in
                withBLEInput(key) { keyBytes, keyCount in
                    switchBLENBLEChallengeResponse(challengeBytes, challengeCount, keyBytes, keyCount, output, capacity)
                }
            }
        }
        guard status == 17, let response else { throw NineEsimCodecError.malformed }
        return response
    }

    static func encode(counter: UInt8, apdu: Data) throws -> [Data] {
        let outputCapacity = apdu.count + 3 + ((apdu.count + 102) / 100) * 3
        let (packed, status) = withBLEOutput(outputCapacity) { output, capacity in
            withBLEInput(apdu) { bytes, count in
                switchBLENBLEEncode(counter, bytes, count, output, capacity)
            }
        }
        guard status > 0, let packed else { throw NineEsimCodecError.malformed }
        var frames: [Data] = []
        var offset = 0
        while offset < packed.count {
            guard offset + 2 <= packed.count else { throw NineEsimCodecError.malformed }
            let frameLength = Int(packed[offset]) << 8 | Int(packed[offset + 1])
            offset += 2
            guard frameLength >= 2, offset + frameLength <= packed.count else {
                throw NineEsimCodecError.malformed
            }
            frames.append(packed.subdata(in: offset..<(offset + frameLength)))
            offset += frameLength
        }
        return frames
    }

    static func checksum<S: Sequence>(_ bytes: S) -> UInt16 where S.Element == UInt8 {
        let data = Data(bytes)
        return withBLEInput(data) { bytes, count in switchBLENBLEChecksum(bytes, count) }
    }
}

private final class NineEsimResponseDecoder {
    private let handle: UnsafeMutableRawPointer

    init() {
        guard let handle = switchBLENBLEDecoderCreate() else { preconditionFailure("Rust NBLE decoder allocation failed") }
        self.handle = handle
    }

    deinit { switchBLENBLEDecoderDestroy(handle) }

    func reset() { switchBLENBLEDecoderReset(handle) }

    func add(_ frame: Data, expectedCounter: UInt8) throws -> Data? {
        let (response, status) = withBLEOutput(65_541) { output, capacity in
            withBLEInput(frame) { bytes, count in
                switchBLENBLEDecoderAdd(handle, bytes, count, expectedCounter, output, capacity)
            }
        }
        if status == 0 { return nil }
        if status == -3 { throw NineEsimCodecError.staleResponse }
        guard status > 0, let response else { throw NineEsimCodecError.malformed }
        return response
    }
}

private final class BeeSimResponseDecoder {
    private let handle: UnsafeMutableRawPointer

    init() {
        guard let handle = switchBLEBeeSIMDecoderCreate() else {
            preconditionFailure("Rust BeeSIM decoder allocation failed")
        }
        self.handle = handle
    }

    deinit { switchBLEBeeSIMDecoderDestroy(handle) }

    func reset() { switchBLEBeeSIMDecoderReset(handle) }

    func add(_ frame: Data) throws -> Data? {
        let (response, status) = withBLEOutput(65_536) { output, capacity in
            withBLEInput(frame) { bytes, count in
                switchBLEBeeSIMDecoderAdd(handle, bytes, count, output, capacity)
            }
        }
        if status == 0 { return nil }
        guard status > 0, let response else { throw NineEsimCodecError.malformed }
        return Data(response.prefix(status - 1))
    }
}

private enum CcidResponseProgress {
    case timeExtension
    case complete(Data)
}

private final class CcidResponseDecoder {
    private let handle: UnsafeMutableRawPointer

    init() {
        guard let handle = switchBLECCIDDecoderCreate() else {
            preconditionFailure("Rust CCID decoder allocation failed")
        }
        self.handle = handle
    }

    deinit { switchBLECCIDDecoderDestroy(handle) }

    func reset() { switchBLECCIDDecoderReset(handle) }

    func add(_ data: Data) throws -> CcidResponseProgress? {
        let (frame, status) = withBLEOutput(65_546) { output, capacity in
            withBLEInput(data) { bytes, count in
                switchBLECCIDDecoderAdd(handle, bytes, count, output, capacity)
            }
        }
        if status == 0 { return nil }
        if status == -5 { return .timeExtension }
        guard status > 0, let frame else { throw NineEsimCodecError.malformed }
        return .complete(Data(frame.prefix(status - 1)))
    }
}

private final class LegacyResponseDecoder {
    private let handle: UnsafeMutableRawPointer

    init() {
        guard let handle = switchBLELegacyDecoderCreate() else {
            preconditionFailure("Rust legacy reader decoder allocation failed")
        }
        self.handle = handle
    }

    deinit { switchBLELegacyDecoderDestroy(handle) }

    func reset() { switchBLELegacyDecoderReset(handle) }

    func add(_ data: Data) throws -> Data? {
        let (response, status) = withBLEOutput(65_536) { output, capacity in
            withBLEInput(data) { bytes, count in
                switchBLELegacyDecoderAdd(handle, bytes, count, output, capacity)
            }
        }
        if status == 0 { return nil }
        guard status > 0, let response else { throw NineEsimCodecError.malformed }
        return Data(response.prefix(status - 1))
    }
}

private func rustCCIDEncodeAPDU(_ apdu: Data) -> Data? {
    let (frame, status) = withBLEOutput(apdu.count + 10) { output, capacity in
        withBLEInput(apdu) { bytes, count in switchBLECCIDEncodeAPDU(bytes, count, output, capacity) }
    }
    return status > 0 ? frame : nil
}

private func rustCCIDExtractAPDU(_ frame: Data) -> Data? {
    let (apdu, status) = withBLEOutput(65_536) { output, capacity in
        withBLEInput(frame) { bytes, count in switchBLECCIDExtractAPDU(bytes, count, output, capacity) }
    }
    guard status > 0, let apdu else { return nil }
    return Data(apdu.prefix(status - 1))
}

private func rustCCIDClaimContinuation(_ frame: Data) -> Data? {
    let (continuation, status) = withBLEOutput(15) { output, capacity in
        withBLEInput(frame) { bytes, count in switchBLECCIDClaimContinuation(bytes, count, output, capacity) }
    }
    return status > 0 ? continuation : nil
}

private func rustLegacyEncodeAPDU(_ apdu: Data) -> Data? {
    let (frame, status) = withBLEOutput(apdu.count + 3) { output, capacity in
        withBLEInput(apdu) { bytes, count in switchBLELegacyEncodeAPDU(bytes, count, output, capacity) }
    }
    return status > 0 ? frame : nil
}

/// Owns the CoreBluetooth central for the lifetime of the application. BLE
/// discovery is not a PC/SC session and is not advertised as an AKA-capable
/// reader until the APDU backend is connected separately.
final class BluetoothReaderManager: NSObject, ObservableObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    static let shared = BluetoothReaderManager()

    @Published private(set) var state: BluetoothReaderState = .idle
    @Published private(set) var errorCode: BluetoothReaderErrorCode?
    @Published private(set) var detail = "正在检查蓝牙…"
    @Published private(set) var devices: [UUID: String] = [:]
    @Published private(set) var connectedID: UUID?

    /// Private bridge lifecycle commands must be bound to the connection that
    /// the caller observed; a delayed disconnect cannot capture a successor.
    func matchesLifecycleTarget(_ id: UUID) -> Bool {
        connectedID == id || desiredID == id
    }
    // Discovery is intentionally independent from the GATT/APDU link.  A
    // user may look for another reader while the selected one is carrying a
    // live AKA/IMS session; treating that re-scan as a disconnect would tear
    // down the reader session below the UI.
    @Published private(set) var isScanning = false
    private let discoveryDeadline = BLEPhaseDeadline()
    var isSearching: Bool { isScanning || centralInitializationPending }

    private var central: CBCentralManager?
    private let centralInitializationDeadline = BLEPhaseDeadline()
    private var centralInitializationPending = false
    private var deferredCentralStateCallback = false
    private var scanRequested = false
    private var peripherals: [UUID: CBPeripheral] = [:]
    // The active APDU session keeps its own CBPeripheral object. Discovery
    // may return a newer object for the same UUID, but that object is adopted
    // only when a fresh connection attempt begins.
    private var latestDiscoveredPeripherals: [UUID: CBPeripheral] = [:]
    // A live advertisement is a fresher object source than CoreBluetooth's
    // retrieved-connected cache, including for readers not yet selected.
    private var advertisedPeripheralIDs = Set<UUID>()
    private struct StagedReader {
        let name: String
        let proto: ESTKProtocol
        let nameIsAdvertised: Bool
    }
    // Discovery may refine a reader while an APDU link is in flight. Its
    // identity and framing are adopted only at the next connection attempt.
    private var stagedReaders: [UUID: StagedReader] = [:]
    private var advertisedServices: [UUID: Set<CBUUID>] = [:]
    private var protocols: [UUID: ESTKProtocol] = [:]
    // A name read from advertisementData is newer than CoreBluetooth's
    // peripheral.name cache. A name obtained only from that cache is not:
    // later nameless advertisements may carry a corrected cached name.
    private var readersWithAdvertisedName = Set<UUID>()

    // ESTKme-RED service/characteristics
    private let estkServiceUUID = CBUUID(string: "4553")

    // BeeSIM (9esim) service/characteristics
    // Service: 0000ae30-0000-1000-8000-00805f9b34fb
    // TX (write): AE01, RX (notify): AE02
    private let beeSimServiceUUID = CBUUID(string: "AE30")

    // 9eSIM B1 NBLE transport (Nordic UART). A NUS-only advertisement is kept
    // as a reader candidate even when the local name is hidden; the authenticated
    // B1 handshake is still required before any card/APDU operation is exposed.
    private let nineEsimServiceUUID = CBUUID(string: "6e400001-b5a3-f393-e0a9-e50e24dcca9e")

    private var notifyCharacteristic: CBCharacteristic?
    private var writeCharacteristic: CBCharacteristic?
    private var notificationSetupStarted = false
    private var cardHandshakeStarted = false
    private var desiredID: UUID?
    private let reconnectPolicy = BLEReconnectPolicy()
    private var manualDisconnect: Bool { reconnectPolicy.stopped }
    private var reconnectWork: DispatchWorkItem? {
        didSet { if reconnectWork == nil { reconnectPolicy.cancelPending() } }
    }
    private let phaseDeadline = BLEPhaseDeadline()
    private let preferredReaderKey = "Proletarius.PreferredESTKReaderID"
    private let targetSelectionPolicy = BLETargetSelectionPolicy()
    private var pendingReply: ((Result<Data, Error>) -> Void)?
    private let ccidResponseDecoder = CcidResponseDecoder()
    private let legacyResponseDecoder = LegacyResponseDecoder()
    private let replyDeadline = BLEPhaseDeadline()
    private var replyTimeout: UInt64?
    private var writeQueue: [Data] = []
    private var writeIndex = 0
    private var writeType: CBCharacteristicWriteType?
    private var writeAwaitingResponse = false

    // BeeSIM response reassembly state
    private let beeSimResponseDecoder = BeeSimResponseDecoder()

    // Authenticated 9eSIM NBLE session state. These values are cleared on
    // every connection teardown and never included in diagnostics.
    private let nineEsimHandshake = NineEsimHandshake()
    private var nineEsimHandshakeStage: NineEsimHandshakeStage? { nineEsimHandshake.stage }
    private var nineEsimHandshakeKey: Data?
    private var nineEsimEarlyAuthenticationAck: Data?
    private var nineEsimHandshakeWriteAwaitingResponse = false
    private var nineEsimResponseDecoder = NineEsimResponseDecoder()
    private var nineEsimKeepaliveWork: DispatchWorkItem?
    private var nineEsimLastActivity = Date()
    private var nineEsimPasswordLookup: (UUID) throws -> String? = NineEsimBluetoothCredentials.password

    private override init() { super.init() }

    // Diagnostic IO is bounded and isolated from BLE callbacks/keepalive.
    private func bleDiagLog(_ msg: String) {
        BoundedBLEDiagnosticLog.shared.enqueue(msg)
    }

    // MARK: - Protocol detection

    private func detectProtocol(name: String) -> ESTKProtocol {
        detectProtocol(name: name, hasNordicUART: false)
    }

    private func detectProtocol(name: String, services: Set<CBUUID>) -> ESTKProtocol {
        detectProtocol(name: name, hasNordicUART: services.contains(nineEsimServiceUUID))
    }

    private func detectProtocol(name: String, hasNordicUART: Bool) -> ESTKProtocol {
        let utf8 = Array(name.utf8)
        let selected = utf8.withUnsafeBufferPointer { bytes in
            switchBLEDetectProtocol(bytes.baseAddress, bytes.count, hasNordicUART ? 1 : 0)
        }
        switch selected {
        case 1: return .ccid
        case 2: return .beeSim
        case 3: return .nineEsim
        default: return .legacy
        }
    }

    /// Presentation only: keep the BLE UUID as the transport/selection key.
    func displayName(forReader reader: String) -> String {
        guard reader.hasPrefix("BLE:"),
              let id = UUID(uuidString: String(reader.dropFirst(4))),
              let name = devices[id] else { return reader }
        if case .nineEsim? = protocols[id], name.lowercased().hasPrefix("9esim") {
            return "9eSIM_B1"
        }
        return name.isEmpty ? reader : name
    }

    func supportsNineEsimCredentials(_ id: UUID) -> Bool {
        protocols[id] == .nineEsim
            || advertisedServices[id]?.contains(nineEsimServiceUUID) == true
            || stagedReaders[id]?.proto == .nineEsim
    }

    /// External APDU clients must wait for both authentication and card-mode
    /// verification. Internal handshake APDUs may run while discovering.
    func isCardChannelReady(for requestedReader: String?) -> Bool {
        guard state == .connected, let connectedID else { return false }
        guard let requestedReader else { return true }
        guard requestedReader.hasPrefix("BLE:"),
              let expectedID = UUID(uuidString: String(requestedReader.dropFirst(4))) else { return false }
        return expectedID == connectedID
    }

    private func clearNineEsimSession() {
        nineEsimKeepaliveWork?.cancel()
        nineEsimKeepaliveWork = nil
        nineEsimHandshake.reset()
        nineEsimPendingHandshakeWrite = nil
        eraseNineEsimHandshakeKey()
        nineEsimEarlyAuthenticationAck = nil
        nineEsimHandshakeWriteAwaitingResponse = false
        nineEsimResponseDecoder = NineEsimResponseDecoder()
    }

    private func eraseNineEsimHandshakeKey() {
        if let count = nineEsimHandshakeKey?.count, count > 0 {
            nineEsimHandshakeKey?.resetBytes(in: 0..<count)
        }
        nineEsimHandshakeKey = nil
    }

    private func currentProtocol() -> ESTKProtocol? {
        let id = connectedID ?? desiredID
        guard let id else { return nil }
        return protocols[id]
    }

    // MARK: - Scan

    private func importSystemConnectedReaders(_ central: CBCentralManager) {
        // Retrieve peripherals for both ESTKme and BeeSIM service UUIDs.
        let allServiceUUIDs = [estkServiceUUID, beeSimServiceUUID, nineEsimServiceUUID]
        for peripheral in central.retrieveConnectedPeripherals(withServices: allServiceUUIDs) {
            let id = peripheral.identifier
            // CoreBluetooth's retrieved object/name can be older than a
            // same-UUID advertisement already staged for the next attempt.
            if advertisedPeripheralIDs.contains(id) { continue }
            latestDiscoveredPeripherals[id] = peripheral
            advertisedServices[id] = Set(allServiceUUIDs)
            // An in-flight or established APDU session owns its identity and
            // framing. A discovery-only re-scan must not replace it with a
            // nil/stale peripheral.name returned by CoreBluetooth.
            if id == connectedID || id == desiredID {
                let hasAdvertisedName = readersWithAdvertisedName.contains(id)
                // Retrieval refreshes the object reference, not an active
                // reader's protocol: peripheral.name is a stale cache.
                let name = devices[id] ?? peripheral.name ?? ""
                let displayName = name.isEmpty ? "eSIM Reader" : name
                stagedReaders[id] = StagedReader(name: displayName,
                                                  proto: protocols[id] ?? detectProtocol(name: displayName),
                                                  nameIsAdvertised: hasAdvertisedName)
                continue
            }
            peripherals[id] = peripheral
            // The same name rule applies to another discovered reader whose
            // advertised LocalName is newer than the retrieved cache.
            if readersWithAdvertisedName.contains(id) { continue }
            let name = peripheral.name ?? ""
            devices[id] = name.isEmpty ? "eSIM Reader" : name
            protocols[id] = detectProtocol(name: name)
            ReaderOperationLogger.write(module: "bluetooth", action: "retrieve_connected",
                                        state: "found", result: "ok")
        }
    }

    private var hasActiveLink: Bool {
        switch state {
        case .connecting, .discovering, .connected, .reconnecting:
            return desiredID != nil || connectedID != nil
        case .idle, .scanning, .disconnected, .failed:
            return false
        }
    }

    private func clearInactiveLinkState(on central: CBCentralManager) {
        // A failed or abandoned attempt may still have a CoreBluetooth
        // connection in flight.  Clear its identity before cancellation so a
        // delayed callback cannot revive it or schedule an unwanted reconnect.
        let staleIDs = Set([desiredID, connectedID].compactMap { $0 })
        phaseDeadline.cancel()
        desiredID = nil
        connectedID = nil
        notifyCharacteristic = nil
        writeCharacteristic = nil
        clearNineEsimSession()
        reconnectWork?.cancel()
        reconnectWork = nil
        for id in staleIDs {
            if let peripheral = peripherals[id] {
                central.cancelPeripheralConnection(peripheral)
            }
        }
    }

    private func clearDiscoveryCache() {
        devices.removeAll()
        peripherals.removeAll()
        latestDiscoveredPeripherals.removeAll()
        advertisedPeripheralIDs.removeAll()
        stagedReaders.removeAll()
        advertisedServices.removeAll()
        protocols.removeAll()
        readersWithAdvertisedName.removeAll()
    }

    private func beginDiscovery(on central: CBCentralManager) {
        guard !AcceptanceBootstrap.isAcceptance else { return }
        importSystemConnectedReaders(central)
        central.scanForPeripherals(withServices: nil,
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: true])
        isScanning = true
        discoveryDeadline.arm(phase: .discovery) { [weak self] action in
            guard action == .stopDiscovery else { return }
            guard let self, self.isScanning else { return }
            self.stopScan()
            if !self.hasActiveLink {
                self.detail = self.devices.isEmpty
                    ? "扫描结束，未发现读卡器。请确认设备已开启蓝牙广播，再重新扫描。"
                    : "扫描结束，发现 \(self.devices.count) 个读卡器。请选择设备连接。"
            }
        }
    }

    private func failBluetoothAuthorization() {
        centralInitializationPending = false
        deferredCentralStateCallback = false
        centralInitializationDeadline.cancel()
        reconnectWork?.cancel()
        reconnectWork = nil
        central?.stopScan()
        finishReply(.failure(BluetoothReaderTransportError(code: .unauthorized)))
        connectedID = nil
        notifyCharacteristic = nil
        writeCharacteristic = nil
        fail(.unauthorized, "请在系统设置中允许 Switch 使用蓝牙")
    }

    private func awaitCentralInitialization() {
        guard centralInitializationPending else { return }
        if CBManager.authorization == .notDetermined {
            detail = "请在系统提示中允许 Switch 使用蓝牙，随后将自动继续扫描…"
            centralInitializationDeadline.arm(phase: .permissionObservation) { [weak self] action in
                guard action == .observePermissionAgain else { return }
                self?.awaitCentralInitialization()
            }
        } else if CBManager.authorization != .allowedAlways {
            failBluetoothAuthorization()
        } else {
            detail = "正在检查蓝牙…"
            if deferredCentralStateCallback, let central {
                deferredCentralStateCallback = false
                centralManagerDidUpdateState(central)
                return
            }
            centralInitializationDeadline.arm(phase: .centralInitialization) { [weak self] action in
                guard action == .failInitialization else { return }
                guard let self, self.centralInitializationPending else { return }
                self.centralInitializationPending = false
                self.deferredCentralStateCallback = false
                self.central = nil
                self.fail(.initializationTimeout, "蓝牙初始化超时，请确认系统蓝牙和应用权限后重新扫描")
            }
        }
    }

    func scan() {
        guard !AcceptanceBootstrap.isAcceptance else {
            fail(.connectFailed, "隔离验收中蓝牙不可用")
            return
        }
        scanRequested = true
        if !hasActiveLink {
            targetSelectionPolicy.beginScan(
                rememberedBytes: UserDefaults.standard.string(forKey: preferredReaderKey))
        }
        if CBManager.authorization != .notDetermined && CBManager.authorization != .allowedAlways {
            failBluetoothAuthorization()
        } else if central == nil {
            errorCode = nil
            state = .idle
            detail = "正在检查蓝牙…"
            centralInitializationPending = true
            deferredCentralStateCallback = false
            awaitCentralInitialization()
            central = CBCentralManager(delegate: self, queue: .main)
        } else if CBManager.authorization == .notDetermined {
            errorCode = nil
            state = .idle
            centralInitializationPending = true
            awaitCentralInitialization()
        } else if let central, central.state == .poweredOn {
            centralInitializationPending = false
            deferredCentralStateCallback = false
            centralInitializationDeadline.cancel()
            if hasActiveLink {
                // Re-scan is discovery-only while a reader is connected,
                // connecting, or automatically reconnecting.  Never clear
                // desiredID/connectedID, characteristics, or reconnectWork
                // here: all of them belong to the active card session.
                beginDiscovery(on: central)
            } else {
                // A terminal failed/disconnected attempt can be discarded so
                // the next connection starts from a clean discovery cache.
                clearInactiveLinkState(on: central)
                clearDiscoveryCache()
                errorCode = nil
                state = .scanning
                detail = "正在扫描蓝牙读卡器…"
                beginDiscovery(on: central)
            }
        } else if let central {
            errorCode = nil
            state = .idle
            detail = "正在检查蓝牙…"
            centralInitializationPending = true
            awaitCentralInitialization()
            switch central.state {
            case .unauthorized, .poweredOff, .unsupported:
                centralManagerDidUpdateState(central)
            default: break
            }
        }
        ReaderOperationLogger.write(module: "bluetooth", action: "scan", state: "requested", result: "pending")
    }

    func stopScan() {
        discoveryDeadline.cancel()
        scanRequested = false
        central?.stopScan()
        isScanning = false
        let wasInitializing = centralInitializationPending
        centralInitializationPending = false
        centralInitializationDeadline.cancel()
        // A consent sheet may outlive this UI sheet. Keep its central so a
        // later grant is observed and the next explicit scan can reuse it.
        if wasInitializing && CBManager.authorization != .notDetermined {
            central = nil
            deferredCentralStateCallback = false
        }
        if wasInitializing || state == .scanning {
            state = .idle
            detail = "蓝牙扫描已停止"
        }
    }

    // MARK: - Connect / Disconnect

    func connect(_ id: UUID) {
        guard !AcceptanceBootstrap.isAcceptance else {
            fail(.connectFailed, "隔离验收中蓝牙不可用")
            return
        }
        guard targetSelectionPolicy.selectExplicitly(id) else {
            fail(.connectFailed, "蓝牙设备标识无效，请重新扫描")
            return
        }
        connectSelected(id)
    }

    private func connectSelected(_ id: UUID) {
        if connectedID == id { return }
        if desiredID == id {
            switch state {
            case .connecting, .discovering, .connected, .reconnecting:
                // A remembered-reader restore, an explicit row click, and a
                // late central-state callback can overlap. Reissuing connect
                // here cancels the active phase deadline and clears NBLE
                // handshake state while CoreBluetooth is still connecting.
                bleDiagLog("connect ignored duplicate request state=\(state)")
                return
            case .idle, .scanning, .disconnected, .failed:
                break
            }
        }
        guard CBManager.authorization == .allowedAlways else {
            if CBManager.authorization != .notDetermined { failBluetoothAuthorization() }
            return
        }
        // Clear stale state from a session that the Go backend may have
        // already torn down via ReconcilePresence during the GATT handshake.
        if let oldID = desiredID, oldID != id, let pending = peripherals[oldID] {
            central?.cancelPeripheralConnection(pending)
        }
        if let oldID = connectedID, oldID != id, let pending = peripherals[oldID] {
            central?.cancelPeripheralConnection(pending)
        }
        phaseDeadline.cancel()
        desiredID = nil
        connectedID = nil
        notifyCharacteristic = nil
        writeCharacteristic = nil
        guard let peripheral = peripherals[id], central?.state == .poweredOn else {
            bleDiagLog("connect: REJECTED peripheral=\(peripherals[id] != nil) poweredOn=\(central?.state == .poweredOn)")
            fail(.connectFailed, "蓝牙设备不可用，请重新扫描")
            return
        }
        bleDiagLog("connect: name=\(devices[id] ?? "?") proto=\(protocols[id] ?? .legacy)")
        reconnectWork?.cancel()
        reconnectPolicy.select()
        desiredID = id
        stopScan()
        peripheral.delegate = self
        startConnectionAttempt(id, detail: "正在连接 \(devices[id] ?? "蓝牙读卡器")…")
        ReaderOperationLogger.write(module: "bluetooth", action: "connect", state: "connecting", result: "pending")
    }

    func disconnect() {
        reconnectPolicy.stop()
        if centralInitializationPending { central = nil }
        centralInitializationPending = false
        deferredCentralStateCallback = false
        centralInitializationDeadline.cancel()
        UserDefaults.standard.removeObject(forKey: preferredReaderKey)
        targetSelectionPolicy.stop()
        let pendingID = connectedID ?? desiredID
        phaseDeadline.cancel()
        desiredID = nil
        connectedID = nil
        notifyCharacteristic = nil
        writeCharacteristic = nil
        clearNineEsimSession()
        reconnectWork?.cancel()
        reconnectWork = nil
        finishReply(.failure(BluetoothReaderTransportError(code: .disconnected)))
        // A cancelled connection may never produce didDisconnect. Publish the
        // terminal state here, after failing any pending handshake callback.
        state = .disconnected
        detail = "蓝牙已断开"
        errorCode = nil
        if let id = pendingID, let peripheral = peripherals[id] {
            central?.cancelPeripheralConnection(peripheral)
        }
        ReaderOperationLogger.write(module: "bluetooth", action: "disconnect", state: "disconnected", result: "ok")
    }

    private func fail(_ code: BluetoothReaderErrorCode, _ message: String) {
        phaseDeadline.cancel()
        discoveryDeadline.cancel()
        central?.stopScan()
        isScanning = false
        if let id = desiredID, protocols[id] == .nineEsim { clearNineEsimSession() }
        errorCode = code
        state = .failed
        detail = message
        ReaderOperationLogger.write(module: "bluetooth", action: "state", state: "failed", result: "error", errorCode: code.rawValue)
    }

    private func armPhaseDeadline(
        for id: UUID,
        phase: BLEOwnerPhase,
        expectedState: BluetoothReaderState,
        code: BluetoothReaderErrorCode,
        message: String
    ) {
        phaseDeadline.arm(phase: phase, target: id.uuidString) { [weak self] action in
            guard action == .failConnectionPhase else { return }
            guard let self, self.desiredID == id, self.state == expectedState else { return }
            self.bleDiagLog("connection phase timeout state=\(expectedState) peripheralState=\(self.peripherals[id].map { String(describing: $0.state) } ?? "missing")")
            self.fail(code, message)
            if let peripheral = self.peripherals[id] {
                self.central?.cancelPeripheralConnection(peripheral)
            }
            self.scheduleReconnect()
        }
    }

    private func startConnectionAttempt(_ id: UUID, detail: String) {
        guard !AcceptanceBootstrap.isAcceptance else { return }
        guard CBManager.authorization == .allowedAlways else {
            if CBManager.authorization != .notDetermined { failBluetoothAuthorization() }
            return
        }
        guard desiredID == id, let central, central.state == .poweredOn else { return }
        if cardCompletionProof?.unresolved.isEmpty == false {
            // CoreBluetooth can reuse the same peripheral object on reconnect.
            // Do not start a new connection/handshake before the previous
            // written card operation receives its disconnect acknowledgement.
            reconnectWork?.cancel()
            reconnectWork = nil
            reconnectAfterCardCompletion = true
            state = .reconnecting
            self.detail = "等待蓝牙确认上一读卡事务已结束…"
            return
        }
        if physicalConnections.values.contains(where: { $0.published && $0.identity.reader == "BLE:" + id.uuidString }) {
            reconnectWork?.cancel(); reconnectWork = nil
            state = .reconnecting
            self.detail = "等待蓝牙确认上一连接已关闭…"
            return
        }
        if let latest = latestDiscoveredPeripherals[id] {
            peripherals[id] = latest
        }
        guard let peripheral = peripherals[id] else { return }
        // The advertised object is now the selected attempt; a later system
        // retrieval may again become a candidate for a *future* attempt.
        advertisedPeripheralIDs.remove(id)
        if let staged = stagedReaders.removeValue(forKey: id) {
            devices[id] = staged.name
            protocols[id] = staged.proto
            if staged.nameIsAdvertised { readersWithAdvertisedName.insert(id) }
            else { readersWithAdvertisedName.remove(id) }
        }
        reconnectWork?.cancel()
        reconnectWork = nil
        phaseDeadline.cancel()
        notifyCharacteristic = nil
        writeCharacteristic = nil
        clearNineEsimSession()
        notificationSetupStarted = false
        cardHandshakeStarted = false
        state = .connecting
        self.detail = detail
        peripheral.delegate = self
        armPhaseDeadline(
            for: id,
            phase: .connecting,
            expectedState: .connecting,
            code: .connectionTimeout,
            message: "蓝牙连接等待设备响应超时，稍后将自动重试")
        bleDiagLog("central.connect requested state=\(state) peripheralState=\(peripheral.state)")
        physicalConnections = physicalConnections.filter { $0.value.published || $0.value.identity.reader != "BLE:" + id.uuidString }
        physicalConnections[ObjectIdentifier(peripheral)] = PhysicalConnection(peripheral: peripheral, identity:
            PhysicalConnectionIdentity(reader: "BLE:" + id.uuidString,
                connectionID: UUID().uuidString, closeOperationID: UUID().uuidString))
        central.connect(peripheral)
    }

    // MARK: - APDU Exchange

    private final class CardCompletionProof {
        var unresolved: [ObjectIdentifier: CBPeripheral] = [:]
        var result: Result<Data, Error>?
        let completion: (Result<Data, Error>) -> Void
        init(completion: @escaping (Result<Data, Error>) -> Void) {
            self.completion = completion
        }
    }
    struct PhysicalConnectionIdentity: Equatable {
        let reader: String
        let connectionID: String
        let closeOperationID: String
    }
    private struct PhysicalConnection {
        let peripheral: CBPeripheral
        let identity: PhysicalConnectionIdentity
        var published = false
    }
    private var physicalConnections: [ObjectIdentifier: PhysicalConnection] = [:]
    var physicalConnectionClosed: ((PhysicalConnectionIdentity) -> Void)?

    /// Captured only from a ready physical connection. Once published to Rust,
    /// its epoch cannot be overwritten by reconnect before didDisconnect.
    func connectionIdentityForBridge() -> PhysicalConnectionIdentity? {
        guard state == .connected, let id = connectedID, let peripheral = peripherals[id],
              peripheral.state == .connected,
              var connection = physicalConnections[ObjectIdentifier(peripheral)] else { return nil }
        connection.published = true
        physicalConnections[ObjectIdentifier(peripheral)] = connection
        return connection.identity
    }

    private func confirmPhysicalConnectionDisconnected(_ peripheral: CBPeripheral) {
        guard peripheral.state == .disconnected,
              let connection = physicalConnections.removeValue(forKey: ObjectIdentifier(peripheral)) else { return }
        physicalConnectionClosed?(connection.identity)
        if !manualDisconnect && desiredID == peripheral.identifier && reconnectWork == nil {
            scheduleReconnect()
        }
    }
    private var cardCompletionProof: CardCompletionProof?
    private var physicalReplyPeripheral: CBPeripheral?
    private var reconnectAfterCardCompletion = false

    /// The bridge's physical receipt must outlive a software timeout. Existing
    /// protocol continuations still receive errors promptly; only this outer
    /// completion waits for a submitted exchange to become physically quiet.
    func performCardOperationWithCompletionProof(
        _ operation: (@escaping (Result<Data, Error>) -> Void) -> Void,
        completion: @escaping (Result<Data, Error>) -> Void
    ) {
        guard cardCompletionProof == nil, pendingReply == nil else {
            completion(.failure(BluetoothReaderTransportError(code: .apduBusy)))
            return
        }
        let proof = CardCompletionProof(completion: completion)
        cardCompletionProof = proof
        operation { [weak self, weak proof] result in
            guard let self, let proof, self.cardCompletionProof === proof else { return }
            proof.result = result
            self.deliverConfirmedCardCompletion()
        }
    }

    private func deliverConfirmedCardCompletion() {
        guard let proof = cardCompletionProof, proof.unresolved.isEmpty,
              let result = proof.result else { return }
        let resumeReconnect = reconnectAfterCardCompletion
        reconnectAfterCardCompletion = false
        cardCompletionProof = nil
        proof.completion(result)
        if resumeReconnect && reconnectWork == nil { scheduleReconnect() }
    }

    private func confirmCardPeripheralDisconnected(_ peripheral: CBPeripheral) {
        cardCompletionProof?.unresolved.removeValue(forKey: ObjectIdentifier(peripheral))
        deliverConfirmedCardCompletion()
    }

    /// The only raw exchange entry point. Callers must be serialized by
    /// the IPC bridge; an in-flight request is never overwritten.
    func exchangeRaw(_ frame: Data, completion: @escaping (Result<Data, Error>) -> Void) {
        guard cardCompletionProof?.unresolved.isEmpty != false else {
            completion(.failure(BluetoothReaderTransportError(code: .apduBusy)))
            return
        }
        guard pendingReply == nil else {
            completion(.failure(BluetoothReaderTransportError(code: .apduBusy)))
            return
        }
        guard let id = connectedID ?? desiredID, let peripheral = peripherals[id],
              peripheral.state == .connected, writeCharacteristic != nil,
              notifyCharacteristic?.isNotifying == true else {
            completion(.failure(BluetoothReaderTransportError(code: .disconnected)))
            return
        }
        pendingReply = completion
        physicalReplyPeripheral = nil
        // Profile download involves long BPP transfers; 120s avoids premature
        // timeout during large profile writes over BLE.
        replyTimeout = replyDeadline.arm(phase: .apdu, target: id.uuidString) { [weak self] action in
            guard action == .expireAPDUObservation else { return }
            self?.invalidateTransport(.apduTimeout)
        }

        let proto = protocols[id] ?? .legacy
        let dataToSend: Data
        var atomicWrites: [Data]? = nil
        switch proto {
        case .beeSim:
            beeSimResponseDecoder.reset()
            guard let encoded = encodeBeeSimFrames(frame, peripheral: peripheral) else {
                // BeeSIM carries frame counts in one byte. Reject oversized
                // requests before writing, without tearing down a healthy link.
                finishReply(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
                return
            }
            dataToSend = encoded
        case .ccid:
            ccidResponseDecoder.reset()
            dataToSend = frame
        case .legacy:
            legacyResponseDecoder.reset()
            dataToSend = frame
        case .nineEsim:
            do {
                let counter = nineEsimHandshake.nextCounter()
                let frames = try NineEsimProtocol.encode(counter: counter, apdu: frame)
                nineEsimResponseDecoder = NineEsimResponseDecoder()
                bleDiagLog("9eSIM NBLE: APDU TX counter=\(counter) bytes=\(frame.count) frames=\(frames.count)")
                atomicWrites = frames
                dataToSend = Data()
            } catch {
                finishReply(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
                return
            }
        }

        // Match ESTK/NekokoLPA2's transport choice: use Write Without Response
        // only when the characteristic does not also offer Write. A writer
        // that supports only Write must be paced by didWriteValue callbacks.
        let supportsWrite = writeCharacteristic!.properties.contains(.write)
        let supportsWriteWithoutResponse = writeCharacteristic!.properties.contains(.writeWithoutResponse)
        guard supportsWrite || supportsWriteWithoutResponse else {
            finishReply(.failure(BluetoothReaderTransportError(code: .characteristicMissing)))
            return
        }
        writeType = (!supportsWrite && supportsWriteWithoutResponse) ? .withoutResponse : .withResponse
        writeAwaitingResponse = false
        let chunkSize = max(1, peripheral.maximumWriteValueLength(for: writeType!))
        if let atomicWrites {
            guard atomicWrites.allSatisfy({ $0.count <= chunkSize }) else {
                finishReply(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
                return
            }
            writeQueue = atomicWrites
        } else {
            writeQueue = stride(from: 0, to: dataToSend.count, by: chunkSize).map { offset in
                dataToSend.subdata(in: offset..<min(offset + chunkSize, dataToSend.count))
            }
        }
        writeIndex = 0
        flushWriteQueue(peripheral)
    }

    func exchangeAPDU(_ apdu: Data, completion: @escaping (Result<Data, Error>) -> Void) {
        guard apdu.count <= 65535 else {
            completion(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
            return
        }
        let id = connectedID ?? desiredID
        guard let id else {
            completion(.failure(BluetoothReaderTransportError(code: .disconnected)))
            return
        }
        let proto = protocols[id] ?? .legacy

        switch proto {
        case .beeSim:
            // BeeSIM: raw APDU goes through the [total, index] framing
            // in exchangeRaw, no additional header needed here.
            exchangeRaw(apdu) { result in
                switch result {
                case .failure(let error): completion(.failure(error))
                case .success(let response):
                    // BeeSIM response is the reassembled payload (no CCID header)
                    completion(.success(response))
                }
            }
        case .nineEsim:
            // NBLE carries the APDU directly inside its authenticated,
            // checksummed transport frames, with no CCID wrapper.
            exchangeRaw(apdu, completion: completion)
        case .ccid:
            guard let frame = rustCCIDEncodeAPDU(apdu) else {
                completion(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
                return
            }
            exchangeRaw(frame) { result in
                switch result {
                case .failure(let error): completion(.failure(error))
                case .success(let response):
                    guard let payload = rustCCIDExtractAPDU(response) else {
                        completion(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
                        return
                    }
                    completion(.success(payload))
                }
            }
        case .legacy:
            guard let frame = rustLegacyEncodeAPDU(apdu) else {
                completion(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
                return
            }
            exchangeRaw(frame, completion: completion)
        }
    }

    /// Rust supplies commands and next-state decisions. The manager only
    /// executes GATT/APDU I/O and reports the exact transport result back.
    private func executeCardWorkflow(
        _ workflow: BLECardWorkflow,
        previous: Result<Data, Error> = .success(Data()),
        completion: @escaping (Int32, Result<Data, Error>) -> Void
    ) {
        let action = workflow.action()
        guard action.status == 1 || action.status == 2 || action.status == 5 || action.status == 6 else {
            completion(action.status, previous)
            return
        }
        if action.status == 6 { bleDiagLog("CCID: applying optional claim continuation") }
        if action.status == 5, case .failure(let error) = previous {
            bleDiagLog("CCID: optional claim continuation failed: \(error.localizedDescription)")
        }
        let reply: (Result<Data, Error>) -> Void = { [weak self] result in
            guard let self, workflow.advance(step: action.step, result: result) else { return }
            self.executeCardWorkflow(workflow, previous: result, completion: completion)
        }
        if action.status == 2 { exchangeAPDU(action.command, completion: reply) }
        else { exchangeRaw(action.command, completion: reply) }
    }

    func powerCycle(completion: @escaping (Result<Data, Error>) -> Void) {
        guard let workflow = BLECardWorkflow(proto: currentProtocol() ?? .legacy, reset: true) else {
            completion(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
            return
        }
        executeCardWorkflow(workflow) { status, result in
            if status == -15 {
                completion(.failure(BluetoothReaderTransportError(code: .resetUnsupported)))
            } else if status == 4 || status == -16 {
                completion(result)
            } else {
                completion(.failure(BluetoothReaderTransportError(code: .apduMalformed)))
            }
        }
    }

    // MARK: - BeeSIM Frame Encoding

    /// Encodes raw APDU data into BeeSIM [total, index, ...payload] frames.
    /// Each frame's payload is at most (MTU - 3 - 2) bytes to fit in one GATT write.
    private func encodeBeeSimFrames(_ data: Data, peripheral: CBPeripheral) -> Data? {
        let mtu = peripheral.maximumWriteValueLength(for: .withoutResponse)
        let outputCapacity = data.count + ((data.count + 17) / 18 + 1) * 2
        let (encoded, status) = withBLEOutput(outputCapacity) { output, capacity in
            withBLEInput(data) { bytes, count in
                switchBLEBeeSIMEncode(bytes, count, mtu, output, capacity)
            }
        }
        guard status > 0, let encoded else { return nil }
        return encoded
    }

    // MARK: - Write Queue

    private func flushWriteQueue(_ peripheral: CBPeripheral) {
        guard let writeCharacteristic, let writeType else { return }
        switch writeType {
        case .withResponse:
            guard !writeAwaitingResponse, writeIndex < writeQueue.count else { return }
            let chunk = writeQueue[writeIndex]
            writeIndex += 1
            writeAwaitingResponse = true
            physicalReplyPeripheral = peripheral
            peripheral.writeValue(chunk, for: writeCharacteristic, type: .withResponse)
        case .withoutResponse:
            while peripheral.canSendWriteWithoutResponse && writeIndex < writeQueue.count {
                physicalReplyPeripheral = peripheral
                peripheral.writeValue(writeQueue[writeIndex], for: writeCharacteristic, type: .withoutResponse)
                writeIndex += 1
            }
            if writeIndex == writeQueue.count {
                writeQueue.removeAll(keepingCapacity: true)
                writeIndex = 0
                self.writeType = nil
            }
        @unknown default:
            invalidateTransport(.writeFailed)
        }
    }

    func peripheralIsReady(toSendWriteWithoutResponse peripheral: CBPeripheral) {
        guard peripherals[peripheral.identifier] === peripheral else { return }
        if let pending = nineEsimPendingHandshakeWrite,
           nineEsimHandshakeStage == .writingChallengeResponse {
            nineEsimPendingHandshakeWrite = nil
            performNineEsimHandshakeWrite(pending, peripheral: peripheral)
            return
        }
        flushWriteQueue(peripheral)
    }

    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if peripherals[peripheral.identifier] === peripheral,
           (connectedID == peripheral.identifier || desiredID == peripheral.identifier),
           characteristic === writeCharacteristic,
           nineEsimHandshakeStage == .writingChallengeResponse,
           nineEsimHandshakeWriteAwaitingResponse {
            nineEsimHandshakeWriteAwaitingResponse = false
            guard error == nil else {
                bleDiagLog("9eSIM NBLE: challenge response GATT write failed")
                failNineEsimHandshake("9eSIM B1 认证响应写入失败")
                return
            }
            bleDiagLog("9eSIM NBLE: challenge response GATT write acknowledged")
            guard nineEsimHandshake.responseWriteCompleted() else {
                failNineEsimHandshake("9eSIM B1 认证写入状态异常")
                return
            }
            if let earlyAck = nineEsimEarlyAuthenticationAck {
                nineEsimEarlyAuthenticationAck = nil
                acceptNineEsimAuthenticationAck(earlyAck, peripheral: peripheral)
            }
            return
        }
        guard peripherals[peripheral.identifier] === peripheral,
              (connectedID == peripheral.identifier || desiredID == peripheral.identifier),
              let selectedWriter = writeCharacteristic,
              characteristic === selectedWriter,
              writeType == .withResponse,
              writeAwaitingResponse,
              pendingReply != nil else { return }
        if error != nil {
            invalidateTransport(.writeFailed)
            return
        }
        writeAwaitingResponse = false
        if writeIndex >= writeQueue.count {
            writeQueue.removeAll(keepingCapacity: true)
            writeIndex = 0
            writeType = nil
            return
        }
        flushWriteQueue(peripheral)
    }

    // MARK: - Response Handling

    private func finishReply(_ result: Result<Data, Error>) {
        guard let callback = pendingReply else { return }
        if case .failure = result, let peripheral = physicalReplyPeripheral {
            // A watchdog/error callback is not a disconnect acknowledgement.
            // Keep the exact peripheral object, not only its reusable UUID.
            cardCompletionProof?.unresolved[ObjectIdentifier(peripheral)] = peripheral
        }
        physicalReplyPeripheral = nil
        pendingReply = nil
        replyDeadline.cancel()
        replyTimeout = nil
        ccidResponseDecoder.reset()
        legacyResponseDecoder.reset()
        beeSimResponseDecoder.reset()
        nineEsimResponseDecoder = NineEsimResponseDecoder()
        writeQueue.removeAll()
        writeIndex = 0
        writeType = nil
        writeAwaitingResponse = false
        callback(result)
    }

    private func invalidateTransport(_ code: BluetoothReaderErrorCode) {
        let id = connectedID ?? desiredID
        if currentProtocol() == .nineEsim {
            bleDiagLog("9eSIM NBLE: APDU transport invalidated code=\(code.rawValue)")
        }
        connectedID = nil
        notifyCharacteristic = nil
        writeCharacteristic = nil
        clearNineEsimSession()
        // Revoke the transport before invoking a handshake continuation. A
        // failed optional claim must not enqueue another APDU on this link.
        finishReply(.failure(BluetoothReaderTransportError(code: code)))
        errorCode = code
        state = .reconnecting
        detail = code == .notificationsFailed
            ? "蓝牙通知通道中断，正在恢复连接…"
            : "蓝牙 APDU 通道异常，正在恢复连接…"
        if let id, let peripheral = peripherals[id] {
            central?.cancelPeripheralConnection(peripheral)
        }
        ReaderOperationLogger.write(module: "bluetooth", action: "exchange", state: "invalidated", result: "error", errorCode: code.rawValue)
        // Do not depend solely on CoreBluetooth delivering didDisconnect: a
        // radio reset can suppress/late-deliver that callback. Scheduling is
        // A later didDisconnect preserves the already scheduled retry.
        scheduleReconnect()
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        // A connection owns one selected notification characteristic. Never
        // accept a value from another service while an APDU is in flight: a
        // peripheral may expose unrelated notify characteristics, and their
        // bytes must not be interpreted as an ESTK/CCID response.
        guard peripherals[peripheral.identifier] === peripheral,
              (connectedID == peripheral.identifier || desiredID == peripheral.identifier),
              let selectedNotify = notifyCharacteristic,
              characteristic === selectedNotify else { return }
        if error != nil {
            if nineEsimHandshakeStage != nil { failNineEsimHandshake("9eSIM B1 通知通道异常") }
            else if pendingReply != nil { invalidateTransport(.notificationsFailed) }
            return
        }
        guard let value = characteristic.value else { return }

        let proto = currentProtocol() ?? .legacy

        if proto == .nineEsim, nineEsimHandshakeStage != nil {
            if nineEsimHandshakeStage == .waitingChallenge {
                let isChallenge = value.count == 17 && value.first == 0xe0
                bleDiagLog(isChallenge
                    ? "9eSIM NBLE: authentication challenge received"
                    : "9eSIM NBLE: ignored pre-auth notification bytes=\(value.count) marker=\(value.first == 0xe0)")
            } else if nineEsimHandshakeStage == .waitingAuthenticationAck {
                bleDiagLog("9eSIM NBLE: authentication acknowledgement notification bytes=\(value.count)")
            }
            handleNineEsimHandshakeNotification(value, peripheral: peripheral)
            return
        }
        guard pendingReply != nil else { return }

        switch proto {
        case .beeSim:
            handleBeeSimNotification(value)
        case .nineEsim:
            handleNineEsimNotification(value)
        case .ccid:
            handleCcidNotification(value)
        case .legacy:
            handleLegacyNotification(value)
        }
    }

    // MARK: - BeeSIM Response Reassembly

    private func handleBeeSimNotification(_ data: Data) {
        do {
            if let response = try beeSimResponseDecoder.add(data) {
                finishReply(.success(response))
            }
        } catch {
            invalidateTransport(.apduMalformed)
        }
    }

    private var nineEsimPendingHandshakeWrite: Data?

    private func handleNineEsimHandshakeNotification(_ data: Data, peripheral: CBPeripheral) {
        guard let stage = nineEsimHandshakeStage else { return }
        switch stage {
        case .waitingChallenge:
            guard data.count == 17, data.first == 0xe0,
                  let key = nineEsimHandshakeKey else { return }
            do {
                let response = try nineEsimHandshake.respond(to: data, key: key)
                sendNineEsimHandshakeResponse(response, peripheral: peripheral)
            } catch {
                failNineEsimHandshake("9eSIM B1 认证挑战无效")
            }
        case .writingChallengeResponse:
            if nineEsimHandshake.classifyAcknowledgement(data) == 2 {
                nineEsimEarlyAuthenticationAck = data
            }
        case .waitingAuthenticationAck:
            guard data.count == 3, data[0] == 0xe0 else { return }
            acceptNineEsimAuthenticationAck(data, peripheral: peripheral)
        }
    }

    private func sendNineEsimHandshakeResponse(_ response: Data, peripheral: CBPeripheral) {
        guard let writer = writeCharacteristic else {
            failNineEsimHandshake("9eSIM B1 写入通道未就绪")
            return
        }
        let supportsWrite = writer.properties.contains(.write)
        let supportsWithoutResponse = writer.properties.contains(.writeWithoutResponse)
        guard supportsWrite || supportsWithoutResponse else {
            failNineEsimHandshake("9eSIM B1 写入特征值不可写")
            return
        }
        let type: CBCharacteristicWriteType = (!supportsWrite && supportsWithoutResponse)
            ? .withoutResponse : .withResponse
        guard response.count <= peripheral.maximumWriteValueLength(for: type) else {
            failNineEsimHandshake("9eSIM B1 蓝牙写入长度不足")
            return
        }
        if type == .withoutResponse && !peripheral.canSendWriteWithoutResponse {
            nineEsimPendingHandshakeWrite = response
            return
        }
        if type == .withResponse { nineEsimHandshakeWriteAwaitingResponse = true }
        peripheral.writeValue(response, for: writer, type: type)
        if type == .withoutResponse, !nineEsimHandshake.responseWriteCompleted() {
            failNineEsimHandshake("9eSIM B1 认证写入状态异常")
        }
    }

    private func performNineEsimHandshakeWrite(_ response: Data, peripheral: CBPeripheral) {
        guard let writer = writeCharacteristic,
              response.count <= peripheral.maximumWriteValueLength(for: .withoutResponse),
              peripheral.canSendWriteWithoutResponse else {
            failNineEsimHandshake("9eSIM B1 蓝牙写入通道不可用")
            return
        }
        peripheral.writeValue(response, for: writer, type: .withoutResponse)
        guard nineEsimHandshake.responseWriteCompleted() else {
            failNineEsimHandshake("9eSIM B1 认证写入状态异常")
            return
        }
    }

    private func acceptNineEsimAuthenticationAck(_ data: Data, peripheral: CBPeripheral) {
        let classification = nineEsimHandshake.classifyAcknowledgement(data)
        guard classification != 0 else { return }
        if classification == 2 {
            nineEsimEarlyAuthenticationAck = data
            return
        }
        guard classification == 1 else {
            eraseNineEsimHandshakeKey()
            failNineEsimHandshake("9eSIM 蓝牙认证失败，请检查密码后手动重连", retryAllowed: false)
            return
        }
        bleDiagLog("9eSIM NBLE: authentication accepted; checking eUICC management mode")
        eraseNineEsimHandshakeKey()
        guard let workflow = BLECardWorkflow(proto: .nineEsim) else {
            failNineEsimHandshake("9eSIM B1 eUICC 模式检查失败")
            return
        }
        executeCardWorkflow(workflow) { [weak self] status, result in
            guard let self else { return }
            if status == 3 {
                self.nineEsimLastActivity = Date()
                self.completeConnection(peripheral)
                self.scheduleNineEsimKeepalive(peripheral)
            } else if status == -14 {
                self.failNineEsimHandshake("9eSIM 当前未处于 eUICC 管理模式，请先切换到管理模式后重新连接")
            } else {
                let message: String
                if case .failure(let error) = result { message = error.localizedDescription }
                else { message = BluetoothReaderErrorCode.apduMalformed.rawValue }
                self.failNineEsimHandshake("9eSIM B1 eUICC 模式检查失败：\(message)")
            }
        }
    }

    private func failNineEsimHandshake(_ message: String, retryAllowed: Bool = true) {
        guard let id = desiredID, let peripheral = peripherals[id] else { return }
        bleDiagLog("9eSIM NBLE: handshake failed stage=\(nineEsimHandshakeStage.map(String.init(describing:)) ?? "mode-check") reason=\(message)")
        if !retryAllowed && reconnectPolicy.rejectAuthentication() {
            // Execute Rust's explicit forget-target action. Preserve the prior
            // selected-target/UserDefaults semantics; never swap credentials.
            desiredID = nil
            targetSelectionPolicy.stop()
            reconnectWork?.cancel()
            reconnectWork = nil
            if UserDefaults.standard.string(forKey: preferredReaderKey) == id.uuidString {
                UserDefaults.standard.removeObject(forKey: preferredReaderKey)
            }
        }
        clearNineEsimSession()
        fail(.connectFailed, message)
        central?.cancelPeripheralConnection(peripheral)
    }

    private func scheduleNineEsimKeepalive(_ peripheral: CBPeripheral) {
        nineEsimKeepaliveWork?.cancel()
        let work = DispatchWorkItem { [weak self, weak peripheral] in
            guard let self, let peripheral,
                  self.connectedID == peripheral.identifier,
                  self.state == .connected,
                  self.currentProtocol() == .nineEsim else { return }
            let idleMilliseconds = UInt64(max(0, Date().timeIntervalSince(self.nineEsimLastActivity) * 1_000))
            let requestPending: UInt8 = self.pendingReply == nil ? 0 : 1
            if switchBLENBLEKeepaliveDue(idleMilliseconds, requestPending) == 1 {
                self.bleDiagLog("9eSIM NBLE: keepalive started")
                self.exchangeAPDU(Data([0x99, 0x98, 0x00, 0x00, 0xfb, 0x00, 0x00])) { [weak self] result in
                    guard let self else { return }
                    switch result {
                    case .success:
                        self.bleDiagLog("9eSIM NBLE: keepalive completed")
                    case .failure:
                        self.bleDiagLog("9eSIM NBLE: keepalive failed")
                        if self.connectedID == peripheral.identifier {
                            self.invalidateTransport(.apduTimeout)
                        }
                    }
                }
            }
            if self.connectedID == peripheral.identifier { self.scheduleNineEsimKeepalive(peripheral) }
        }
        nineEsimKeepaliveWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 12, execute: work)
    }

    private func handleNineEsimNotification(_ frame: Data) {
        guard pendingReply != nil else { return }
        let counter = nineEsimHandshake.expectedResponseCounter
        do {
            if let response = try nineEsimResponseDecoder.add(frame, expectedCounter: counter) {
                nineEsimLastActivity = Date()
                bleDiagLog("9eSIM NBLE: APDU RX counter=\(counter) bytes=\(response.count)")
                finishReply(.success(response))
            }
        } catch NineEsimCodecError.staleResponse {
            bleDiagLog("9eSIM NBLE: discarded stale APDU response")
            nineEsimResponseDecoder = NineEsimResponseDecoder()
        } catch {
            invalidateTransport(.apduMalformed)
        }
    }

    // MARK: - CCID Response Reassembly

    private func handleCcidNotification(_ data: Data) {
        do {
            guard let progress = try ccidResponseDecoder.add(data) else { return }
            switch progress {
            case .timeExtension:
                return
            case .complete(let frame):
                finishReply(.success(frame))
            }
        } catch {
            invalidateTransport(.apduMalformed)
        }
    }

    /// ESTKme-RED CCID firmware can request a short, optional continuation
    /// after its initial claim control exchange. The reply passed here is the
    /// complete CCID frame returned by `exchangeRaw`.
    private func ccidClaimContinuationFrame(for response: Data) -> Data? {
        rustCCIDClaimContinuation(response)
    }

    // MARK: - Legacy Response Reassembly

    private func handleLegacyNotification(_ data: Data) {
        do {
            if let response = try legacyResponseDecoder.add(data) {
                finishReply(.success(response))
            }
        } catch {
            invalidateTransport(.apduMalformed)
        }
    }

    // MARK: - Reconnect

    private func scheduleReconnect() {
        guard CBManager.authorization == .allowedAlways else {
            if CBManager.authorization != .notDetermined { failBluetoothAuthorization() }
            return
        }
        guard !manualDisconnect, let id = desiredID, central?.state == .poweredOn else { return }
        guard let retry = reconnectPolicy.schedule() else { return }
        phaseDeadline.cancel()
        reconnectWork?.cancel()
        let delay = retry.delay
        state = .reconnecting
        detail = "蓝牙断开，\(Int(delay)) 秒后重连…"
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.desiredID == id,
                  self.reconnectPolicy.fire(epoch: retry.epoch, ticket: retry.ticket) else { return }
            // Consumed timers are not pending work. A missing poweredOff
            // callback must not make later poweredOn retain a dead timer.
            self.reconnectWork = nil
            guard CBManager.authorization == .allowedAlways else {
                if CBManager.authorization != .notDetermined { self.failBluetoothAuthorization() }
                return
            }
            self.startConnectionAttempt(id, detail: "正在重新连接 \(self.devices[id] ?? "蓝牙读卡器")…")
        }
        reconnectWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
        ReaderOperationLogger.write(module: "bluetooth", action: "reconnect", state: "scheduled", result: "pending")
    }

    // MARK: - CBCentralManagerDelegate

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        // A retired central can still deliver a delayed callback after a
        // timeout or sheet dismissal. Only the current attempt owns the UI.
        guard self.central === central || (self.central == nil && centralInitializationPending) else { return }
        if central.state == .poweredOff, cardCompletionProof != nil {
            // A manual disconnect may already have cleared every UI binding.
            // Still consume this system proof for the outstanding card job.
            finishReply(.failure(BluetoothReaderTransportError(code: .poweredOff)))
            cardCompletionProof?.unresolved.removeAll()
            deliverConfirmedCardCompletion()
        }
        // The retained central can report a late permission result after the
        // sheet closes; it must not start discovery until scan() is requested.
        guard scanRequested || desiredID != nil || connectedID != nil || centralInitializationPending else { return }
        if CBManager.authorization == .notDetermined {
            centralInitializationPending = true
            deferredCentralStateCallback = true
            awaitCentralInitialization()
            return
        }
        if CBManager.authorization != .allowedAlways {
            failBluetoothAuthorization()
            return
        }
        deferredCentralStateCallback = false
        if centralInitializationPending && (central.state == .unknown || central.state == .resetting) {
            awaitCentralInitialization()
            return
        }
        centralInitializationPending = false
        centralInitializationDeadline.cancel()
        switch central.state {
        case .poweredOn:
            errorCode = nil
            if connectedID == nil {
                if let id = desiredID, peripherals[id] != nil {
                    switch state {
                    case .connecting, .discovering, .connected:
                        // Do not restart an in-flight attempt just because
                        // CoreBluetooth delivered a duplicate poweredOn event.
                        bleDiagLog("central poweredOn retained in-flight state=\(state)")
                        return
                    case .reconnecting where reconnectWork != nil:
                        bleDiagLog("central poweredOn retained scheduled reconnect")
                        return
                    case .idle, .scanning, .reconnecting, .disconnected, .failed:
                        break
                    }
                    startConnectionAttempt(id, detail: "蓝牙已恢复，正在重新连接…")
                    return
                }
            }
            if scanRequested && connectedID == nil && desiredID == nil {
                if state == .scanning {
                    bleDiagLog("central poweredOn retained active reader scan")
                    return
                }
                // A cached CBPeripheral from retrievePeripherals can remain in
                // `.connecting` without a delegate callback when the reader is
                // no longer advertising that session. Confirm a fresh BLE
                // advertisement first, then connect the peripheral object
                // delivered by didDiscover instead of trusting the cache.
                let rememberedBytes = UserDefaults.standard.string(forKey: preferredReaderKey)
                targetSelectionPolicy.beginScan(rememberedBytes: rememberedBytes)
                state = .scanning
                detail = rememberedBytes?.isEmpty != false
                    ? "正在扫描蓝牙读卡器…"
                    : "正在确认已记忆的读卡器广播…"
                clearDiscoveryCache()
                beginDiscovery(on: central)
            }
        case .unauthorized:
            failBluetoothAuthorization()
        case .poweredOff:
            finishReply(.failure(BluetoothReaderTransportError(code: .poweredOff)))
            connectedID = nil
            notifyCharacteristic = nil
            writeCharacteristic = nil
            fail(.poweredOff, "请开启 Mac 蓝牙")
        case .unsupported:
            finishReply(.failure(BluetoothReaderTransportError(code: .unsupported)))
            connectedID = nil
            notifyCharacteristic = nil
            writeCharacteristic = nil
            fail(.unsupported, "本机不支持蓝牙低功耗")
        default: detail = "等待蓝牙就绪…"
        }
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        let id = peripheral.identifier
        // CoreBluetooth's cached peripheral.name can lag a newer advertised
        // local name. Later name-less packets must not flip the UI back or
        // downgrade a previously resolved APDU framing protocol.
        let advertisedName = (advertisementData[CBAdvertisementDataLocalNameKey] as? String)
            .flatMap { $0.isEmpty ? nil : $0 }
        let stagedAdvertisedName = stagedReaders[id].flatMap { $0.nameIsAdvertised ? $0.name : nil }
        let knownAdvertisedName = stagedAdvertisedName
            ?? (readersWithAdvertisedName.contains(id) ? devices[id] : nil)
        let cachedName = peripheral.name.flatMap { $0.isEmpty ? nil : $0 }
        let name = advertisedName ?? knownAdvertisedName ?? cachedName ?? devices[id] ?? ""
        let normalized = name.lowercased()
        let services = Set((advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID]) ?? [])
        let hasNineEsimNUS = services.contains(nineEsimServiceUUID)

        // Match NekokoLPA2: check for device names containing estkme, esim_writer,
        // beesim, or 9esim; also match ESTKme service UUID 0x4553.
        let nameMatches = normalized.contains("estkme")
            || normalized.contains("estkred")
            || normalized.contains("beesim")
            || normalized.contains("9esim")
            || normalized.contains("esim_writer")
            || normalized.contains("simlink")
        let serviceMatches = services.contains(estkServiceUUID)
            || services.contains(beeSimServiceUUID)
            || hasNineEsimNUS
        guard nameMatches || serviceMatches else { return }

        // Keep a newly discovered object for the next connection attempt,
        // without replacing the object bound to an active GATT/APDU exchange.
        latestDiscoveredPeripherals[id] = peripheral
        advertisedPeripheralIDs.insert(id)
        advertisedServices[id] = services
        let displayName = name.isEmpty ? (hasNineEsimNUS ? "9eSIM B1" : "eSIM Reader") : name
        // A connection attempt pins its protocol before GATT discovery. All
        // later same-UUID advertisements, even before claim, belong to the
        // next attempt rather than the active response parser.
        if id == connectedID || id == desiredID {
            let knownName = stagedReaders[id]?.name ?? devices[id]
            let canRefineMetadata = nameMatches
                && (advertisedName != nil || knownName == nil || knownName == "eSIM Reader")
            let nextName = canRefineMetadata ? displayName : (knownName ?? displayName)
            let nextProtocol = canRefineMetadata
                ? detectProtocol(name: nextName, services: services)
                : (stagedReaders[id]?.proto ?? protocols[id] ?? detectProtocol(name: nextName))
            stagedReaders[id] = StagedReader(name: nextName,
                                              proto: nextProtocol,
                                              nameIsAdvertised: (canRefineMetadata && advertisedName != nil)
                                                  || stagedReaders[id]?.nameIsAdvertised == true
                                                  || readersWithAdvertisedName.contains(id))
            return
        }
        if id != desiredID { peripherals[id] = peripheral }
        if advertisedName != nil { readersWithAdvertisedName.insert(id) }
        protocols[id] = detectProtocol(name: displayName, services: services)
        if devices[id] != displayName {
            devices[id] = displayName
            bleDiagLog("didDiscover accepted: proto=\(protocols[id] ?? .legacy)")
            ReaderOperationLogger.write(module: "bluetooth", action: "discover", state: "found", result: "ok")
        }
        if state == .scanning,
           desiredID == nil,
           connectedID == nil,
           targetSelectionPolicy.acceptsObservedCandidate(id) {
            bleDiagLog("Rust selected remembered reader after fresh advertisement")
            connectSelected(id)
        }
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        if cardCompletionProof?.unresolved.isEmpty == false {
            // Also fence a previously queued connection callback: it must not
            // begin authentication on top of an unconfirmed old card command.
            central.cancelPeripheralConnection(peripheral)
            return
        }
        guard desiredID == peripheral.identifier else {
            central.cancelPeripheralConnection(peripheral)
            bleDiagLog("didConnect ignored stale peripheral=\(peripheral.identifier)")
            return
        }
        guard peripherals[peripheral.identifier] === peripheral else {
            bleDiagLog("didConnect ignored stale peripheral object")
            // If the old object connected after its replacement was selected,
            // retire it without cancelling an already connected new object.
            if peripheral.state == .connected,
               peripherals[peripheral.identifier]?.state != .connected {
                central.cancelPeripheralConnection(peripheral)
            }
            peripheral.delegate = nil
            return
        }
        let name = devices[peripheral.identifier] ?? "?"
        let proto = protocols[peripheral.identifier] ?? .legacy
        reconnectWork?.cancel()
        reconnectWork = nil
        phaseDeadline.cancel()
        bleDiagLog("didConnect: name=\(name) proto=\(proto)")
        state = .discovering
        detail = "正在发现蓝牙读卡器服务…"
        peripheral.delegate = self
        armPhaseDeadline(
            for: peripheral.identifier,
            phase: .serviceDiscovery,
            expectedState: .discovering,
            code: .serviceDiscoveryTimeout,
            message: "蓝牙连接已建立，但读取读卡器服务超时，稍后将自动重试")
        peripheral.discoverServices(nil)
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        guard peripherals[peripheral.identifier] === peripheral,
              desiredID == peripheral.identifier, state == .connecting else {
            bleDiagLog("didFailToConnect ignored stale peripheral=\(peripheral.identifier)")
            return
        }
        phaseDeadline.cancel()
        bleDiagLog("didFailToConnect: error=\(error?.localizedDescription ?? "nil")")
        fail(.connectFailed, "连接失败：\(error?.localizedDescription ?? "设备未响应")")
        scheduleReconnect()
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        guard peripheral.state == .disconnected else { return }
        // Manual cancellation or replacement may already have cleared the UI
        // binding. Its real delegate event still retires that exact old job.
        defer {
            confirmCardPeripheralDisconnected(peripheral)
            confirmPhysicalConnectionDisconnected(peripheral)
        }
        guard peripherals[peripheral.identifier] === peripheral,
              desiredID == peripheral.identifier || connectedID == peripheral.identifier else { return }
        if currentProtocol() == .nineEsim {
            let nsError = error as NSError?
            bleDiagLog("9eSIM NBLE: peripheral disconnected unexpectedly error=\(nsError.map { "\($0.domain):\($0.code)" } ?? "none")")
        }
        let retryAlreadyScheduled = !manualDisconnect && reconnectWork != nil
        phaseDeadline.cancel()
        if !retryAlreadyScheduled {
            reconnectWork?.cancel()
            reconnectWork = nil
        }
        finishReply(.failure(BluetoothReaderTransportError(code: .disconnected)))
        clearNineEsimSession()
        connectedID = nil
        notifyCharacteristic = nil
        writeCharacteristic = nil
        if retryAlreadyScheduled {
            state = .reconnecting
            detail = "蓝牙连接中断，等待自动重试…"
        } else {
            state = .disconnected
            detail = error.map { "蓝牙断开：\($0.localizedDescription)" } ?? "蓝牙已断开"
        }
        if !manualDisconnect {
            errorCode = .disconnected
            ReaderOperationLogger.write(module: "bluetooth", action: "disconnect", state: "unexpected", result: "error", errorCode: BluetoothReaderErrorCode.disconnected.rawValue)
            if !retryAlreadyScheduled {
                scheduleReconnect()
            }
        }
    }

    // MARK: - CBPeripheralDelegate

    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard peripherals[peripheral.identifier] === peripheral,
              desiredID == peripheral.identifier, state == .discovering,
              !notificationSetupStarted,
              peripheral.state == .connected else {
            bleDiagLog("didDiscoverServices ignored stale peripheral=\(peripheral.identifier) state=\(state)")
            return
        }
        phaseDeadline.cancel()
        if let error {
            bleDiagLog("didDiscoverServices ERROR: \(error.localizedDescription)")
            fail(.serviceMissing, "发现蓝牙服务失败：\(error.localizedDescription)")
            central?.cancelPeripheralConnection(peripheral)
            return
        }
        guard let services = peripheral.services, !services.isEmpty else {
            bleDiagLog("didDiscoverServices: NO SERVICES")
            fail(.serviceMissing, "设备没有暴露任何蓝牙服务")
            central?.cancelPeripheralConnection(peripheral)
            return
        }
        let discoveredServiceUUIDs = Set(services.map(\.uuid))
        let readerName = devices[peripheral.identifier] ?? peripheral.name ?? ""
        let proto = detectProtocol(name: readerName, services: discoveredServiceUUIDs)
        protocols[peripheral.identifier] = proto
        bleDiagLog("didDiscoverServices: \(services.count) services: \(services.map { $0.uuid.uuidString }) proto=\(proto)")

        // All protocols use full discovery + cross-service characteristic search.
        gattSelection = BLEGATTSelection(proto: proto, services: services.count)
        armPhaseDeadline(
            for: peripheral.identifier,
            phase: .characteristicDiscovery,
            expectedState: .discovering,
            code: .characteristicDiscoveryTimeout,
            message: "蓝牙读卡器服务已找到，但特征值发现超时，稍后将自动重试")
        for service in services {
            peripheral.discoverCharacteristics(nil, for: service)
        }
    }

    // BeeSIM characteristic search: since we discover ALL services (the
    // 9esim device may use non-standard service UUIDs), this callback fires
    // once per service. We accumulate candidates across calls and only
    // proceed when both TX and RX are found (or all services are done).
    private var gattSelection: BLEGATTSelection?

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard peripherals[peripheral.identifier] === peripheral,
              desiredID == peripheral.identifier, state == .discovering,
              !notificationSetupStarted,
              peripheral.state == .connected else {
            bleDiagLog("didDiscoverCharacteristics ignored stale peripheral=\(peripheral.identifier) state=\(state)")
            return
        }
        armPhaseDeadline(
            for: peripheral.identifier,
            phase: .characteristicDiscovery,
            expectedState: .discovering,
            code: .characteristicDiscoveryTimeout,
            message: "蓝牙读卡器特征值发现超时，稍后将自动重试")
        let proto = protocols[peripheral.identifier] ?? .legacy
        let readerName = devices[peripheral.identifier] ?? peripheral.name ?? ""
        if let error {
            bleDiagLog("didDiscoverCharacteristics ERROR: svc=\(service.uuid.uuidString) \(error.localizedDescription)")
            fail(.characteristicMissing, "读取蓝牙特征失败：\(error.localizedDescription)")
            central?.cancelPeripheralConnection(peripheral)
            return
        }

        let charInfo = (service.characteristics ?? []).map { c -> String in
            let uuid = c.uuid.uuidString
            let props: [String] = [
                c.properties.contains(.read) ? "R" : "",
                c.properties.contains(.write) ? "W" : "",
                c.properties.contains(.writeWithoutResponse) ? "w" : "",
                c.properties.contains(.notify) ? "N" : "",
                c.properties.contains(.indicate) ? "I" : "",
            ].filter { !$0.isEmpty }
            return "\(uuid)(\(props.joined()))"
        }
        bleDiagLog("didDiscoverCharacteristics: svc=\(service.uuid.uuidString) chars=\(charInfo) proto=\(proto) remaining=\((gattSelection?.remaining ?? 0)-1)")

        if gattSelection == nil { gattSelection = BLEGATTSelection(proto: proto, services: 0) }
        guard let selection = gattSelection else { return }
        let result = selection.observe(service, alreadyBound: notifyCharacteristic != nil && writeCharacteristic != nil)
        if result.updates {
            writeCharacteristic = result.writer
            notifyCharacteristic = result.notifier
        }
        switch result.status {
        case 0, 2: return
        case -1:
            bleDiagLog("BeeSIM: FAILED no TX/RX found")
            fail(.characteristicMissing, "设备不支持蓝牙 APDU 通道（未找到可写/可通知特征值）")
        case -2:
            bleDiagLog("legacy/ccid: FAILED no 544b/6d65 or AE01/AE02 or any W/N found")
            fail(.characteristicMissing, "设备不支持蓝牙 APDU 通道（未找到 544b/6d65 或可写/可通知特征值）")
        case -3, -4:
            bleDiagLog(result.status == -3 ? "CCID: FAILED missing ESTKme service 4553"
                : "CCID: FAILED missing exact 4553/544B/6D65 characteristics")
            fail(.characteristicMissing, "ESTKme-RED 未找到 4553 / 544B / 6D65 蓝牙 APDU 通道")
        case -5:
            fail(.characteristicMissing, "9eSIM B1 未找到 Nordic UART 蓝牙服务")
        case -6:
            fail(.characteristicMissing, "9eSIM B1 未找到 Nordic UART 收发特征值")
        case 1: break
        default:
            fail(.characteristicMissing, "设备不支持蓝牙 APDU 通道（特征值选择未确认）")
        }
        guard result.status == 1 else {
            central?.cancelPeripheralConnection(peripheral)
            return
        }
        if let wc = writeCharacteristic, let nc = notifyCharacteristic {
            switch proto {
            case .beeSim: bleDiagLog("BeeSIM: TX=\(wc.uuid.uuidString) RX=\(nc.uuid.uuidString)")
            case .legacy: bleDiagLog("legacy/ccid: TX=\(wc.uuid.uuidString) RX=\(nc.uuid.uuidString)")
            case .ccid: bleDiagLog("CCID: TX=\(wc.uuid.uuidString) RX=\(nc.uuid.uuidString)")
            case .nineEsim:
                nineEsimHandshake.start()
                nineEsimResponseDecoder = NineEsimResponseDecoder()
                do {
                    let savedPassword = try nineEsimPasswordLookup(peripheral.identifier)
                    let password = try NineEsimProtocol.resolvedPassword(
                        deviceName: readerName,
                        storedPassword: savedPassword
                    )
                    nineEsimHandshakeKey = try NineEsimProtocol.deriveKey(deviceName: readerName, password: password)
                } catch {
                    failNineEsimHandshake("无法读取 9eSIM 蓝牙认证密码或识别设备标识，请检查钥匙串与设备名称", retryAllowed: false)
                    return
                }
                bleDiagLog("9eSIM NBLE: Nordic UART characteristics selected")
            }
        }

        guard let nc = notifyCharacteristic, writeCharacteristic != nil else {
            return
        }
        // Other services may complete after the APDU channel was selected.
        // They cannot replace it or restart a GATT deadline during card I/O.
        notificationSetupStarted = true
        armPhaseDeadline(
            for: peripheral.identifier,
            phase: .notification,
            expectedState: .discovering,
            code: .notificationTimeout,
            message: (protocols[peripheral.identifier] ?? .legacy) == .nineEsim
                ? "9eSIM B1 认证或 eUICC 模式检查超时，稍后将自动重试"
                : "蓝牙读卡器通知通道未就绪，稍后将自动重试")
        peripheral.setNotifyValue(true, for: nc)
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        guard peripherals[peripheral.identifier] === peripheral,
              desiredID == peripheral.identifier,
              state == .discovering || state == .connected,
              peripheral.state == .connected,
              let selectedNotify = notifyCharacteristic,
              characteristic === selectedNotify else { return }
        bleDiagLog("didUpdateNotificationState: char=\(characteristic.uuid.uuidString) notifying=\(characteristic.isNotifying) error=\(error?.localizedDescription ?? "nil")")
        guard error == nil, characteristic.isNotifying else {
            invalidateTransport(.notificationsFailed)
            return
        }

        let proto = protocols[peripheral.identifier] ?? .legacy
        if proto == .nineEsim {
            // B1 may emit the challenge immediately after CCCD activation,
            // before this acknowledgement callback arrives. The handshake
            // stage was armed before setNotifyValue, so that packet is safe.
            guard state == .discovering else { return }
            bleDiagLog("9eSIM NBLE: notifications enabled; waiting for authentication challenge")
            cardHandshakeStarted = true
            return
        }
        phaseDeadline.cancel()

        // Duplicate notify acknowledgements must not repeat claim/power-on,
        // including while a previous handshake APDU is still outstanding.
        guard state == .discovering, !cardHandshakeStarted else { return }
        cardHandshakeStarted = true

        guard let workflow = BLECardWorkflow(proto: proto) else {
            fail(.connectFailed, "蓝牙卡片初始化失败")
            central?.cancelPeripheralConnection(peripheral)
            return
        }
        executeCardWorkflow(workflow) { [weak self] status, result in
            guard let self else { return }
            if status == 3 {
                self.completeConnection(peripheral, detail: proto == .beeSim ? "BeeSIM 蓝牙卡片通道已就绪" : nil)
            } else {
                let prefix: String
                switch status {
                case -10: prefix = "ESTK 占用失败"
                case -12: prefix = "BeeSIM 初始化失败"
                default: prefix = "ESTK 上电失败"
                }
                let message: String
                if case .failure(let error) = result { message = error.localizedDescription }
                else { message = BluetoothReaderErrorCode.apduMalformed.rawValue }
                self.fail(.connectFailed, "\(prefix)：\(message)")
                self.central?.cancelPeripheralConnection(peripheral)
            }
        }
    }

    private func completeConnection(_ peripheral: CBPeripheral, detail message: String? = nil) {
        phaseDeadline.cancel()
        if targetSelectionPolicy.mayPersist(peripheral.identifier) {
            UserDefaults.standard.set(peripheral.identifier.uuidString, forKey: preferredReaderKey)
        }
        reconnectPolicy.ready()
        errorCode = nil
        detail = message ?? "蓝牙卡片通道已就绪"
        state = .connected
        connectedID = peripheral.identifier
        if currentProtocol() == .nineEsim {
            bleDiagLog("9eSIM NBLE: authentication and management mode verified; APDU channel ready")
        }
        ReaderOperationLogger.write(module: "bluetooth", action: "connect", state: "card_powered", result: "ok")
    }
}

struct BluetoothReaderTransportError: LocalizedError {
    let code: BluetoothReaderErrorCode
    var errorDescription: String? { code.rawValue }
}

/// The UI consumes this service, not CBCentralManager or CBPeripheral.
final class BluetoothReaderService: ObservableObject {
    private let manager: BluetoothReaderManager
    private var subscription: AnyCancellable?

    init(manager: BluetoothReaderManager = .shared) {
        self.manager = manager
        subscription = manager.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
    }

    func displayName(forReader reader: String) -> String { manager.displayName(forReader: reader) }
    func supportsNineEsimCredentials(_ id: UUID) -> Bool { manager.supportsNineEsimCredentials(id) }
    func nineEsimPassword(for id: UUID) throws -> String? { try NineEsimBluetoothCredentials.password(for: id) }
    func saveNineEsimPassword(_ password: String?, for id: UUID) throws {
        try NineEsimBluetoothCredentials.save(password, for: id)
    }
    var statusText: String { manager.detail }
    var isSearching: Bool { manager.isSearching }
    var devices: [UUID: String] { manager.devices }
    var connectedID: UUID? { manager.connectedID }
    var errorCode: BluetoothReaderErrorCode? { manager.errorCode }
    func scan() { manager.scan() }
    func stopScan() { manager.stopScan() }
    func connect(_ id: UUID) { manager.connect(id) }
    func disconnect() { manager.disconnect() }
}

private enum ReaderOperationLogger {
    static func write(module: String, action: String, state: String, result: String, errorCode: String? = nil) {
        var event: [String: String] = [
            "timestamp": ISO8601DateFormatter().string(from: Date()),
            "module": module, "action": action, "state": state, "result": result
        ]
        if let errorCode { event["errorCode"] = errorCode }
        guard let bytes = try? JSONSerialization.data(withJSONObject: event),
              let line = String(data: bytes, encoding: .utf8) else { return }
        NSLog("%@", line)
    }
}
