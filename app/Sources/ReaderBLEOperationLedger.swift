import Foundation

// These functions are linked through the BLE crate embedded in the app's Rust
// audio archive. Rust owns all receipt state and hashes transient APDU input.
@_silgen_name("switch_ble_ledger_new")
private func ledgerNew(_ capacity: Int, _ bridgeID: UnsafePointer<CChar>) -> UnsafeMutableRawPointer?
@_silgen_name("switch_ble_ledger_call")
private func ledgerCall(_ handle: UnsafeMutableRawPointer, _ request: UnsafePointer<UInt8>, _ length: Int) -> UnsafeMutablePointer<CChar>?
@_silgen_name("switch_ble_ledger_string_free")
private func ledgerStringFree(_ value: UnsafeMutablePointer<CChar>)
@_silgen_name("switch_ble_ledger_free")
private func ledgerFree(_ handle: UnsafeMutableRawPointer)

/// Swift value adapter for Rust's physical completion ledger. Card work and
/// main-queue dispatch never run inside the Rust metadata mutex.
final class ReaderBLEOperationLedger {
    struct Fingerprint: Equatable {
        let action: String
        let reader: String?
        let hex: String?
        var connectionID: String? = nil
    }

    struct Result: Equatable {
        let ok: Bool
        var hex: String? = nil
        var errorCode: String? = nil
    }

    struct Snapshot: Equatable {
        let operationID: String
        let bridgeID: String
        let terminal: Bool
        let result: Result
        var connectionID: String? = nil
        var reader: String? = nil
        var closed: Bool? = nil
    }

    struct Ticket: Equatable {
        let operationID: String
        fileprivate let incarnation: UInt64
        fileprivate let ledgerNonce: String
    }

    enum Admission {
        case dispatch(Ticket)
        case reply(Snapshot)
    }

    private struct WireFingerprint: Encodable {
        let action: String
        let reader: String?
        let hex: String?
        let connectionID: String?
        enum CodingKeys: String, CodingKey {
            case action, reader, hex
            case connectionID = "connection_id"
        }
    }

    private struct WireResult: Codable {
        let ok: Bool
        let hex: String?
        let errorCode: String?
        enum CodingKeys: String, CodingKey {
            case ok, hex
            case errorCode = "error_code"
        }
        init(_ result: Result) {
            ok = result.ok; hex = result.hex; errorCode = result.errorCode
        }
        var value: Result { Result(ok: ok, hex: hex, errorCode: errorCode) }
    }

    private struct WireSnapshot: Decodable {
        let operationID: String
        let bridgeID: String
        let terminal: Bool
        let result: WireResult
        let connectionID: String?
        let reader: String?
        let closed: Bool?
        enum CodingKeys: String, CodingKey {
            case operationID = "operation_id", bridgeID = "bridge_id", terminal, result
            case connectionID = "connection_id", reader, closed
        }
        var value: Snapshot {
            Snapshot(operationID: operationID, bridgeID: bridgeID, terminal: terminal,
                     result: result.value, connectionID: connectionID, reader: reader, closed: closed)
        }
    }

    private struct WireResponse: Decodable {
        let dispatch: Bool
        let accepted: Bool
        let incarnation: UInt64?
        let ledgerNonce: String?
        let snapshot: WireSnapshot?
        enum CodingKeys: String, CodingKey {
            case dispatch, accepted, incarnation, snapshot
            case ledgerNonce = "ledger_nonce"
        }
    }

    private struct WireCommand: Encodable {
        let command: String
        var operationID: String? = nil
        var bridgeID: String? = nil
        var fingerprint: WireFingerprint? = nil
        var rejection: WireResult? = nil
        var incarnation: UInt64? = nil
        var ledgerNonce: String? = nil
        var result: WireResult? = nil
        var reader: String? = nil
        var connectionID: String? = nil
        enum CodingKeys: String, CodingKey {
            case command, fingerprint, rejection, incarnation, result, reader
            case ledgerNonce = "ledger_nonce"
            case operationID = "operation_id", bridgeID = "bridge_id", connectionID = "connection_id"
        }
    }

    let bridgeID: String
    private let handle: UnsafeMutableRawPointer

    init(capacity: Int = 128, bridgeID: String = UUID().uuidString) {
        precondition(capacity > 0)
        self.bridgeID = bridgeID
        guard let handle = bridgeID.withCString({ ledgerNew(capacity, $0) }) else {
            preconditionFailure("Rust BLE operation ledger unavailable")
        }
        self.handle = handle
    }

    deinit { ledgerFree(handle) }

    private func call(_ command: WireCommand) -> WireResponse? {
        guard let data = try? JSONEncoder().encode(command) else { return nil }
        return data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress?.assumingMemoryBound(to: UInt8.self),
                  let response = ledgerCall(handle, base, raw.count) else { return nil }
            defer { ledgerStringFree(response) }
            return try? JSONDecoder().decode(WireResponse.self, from: Data(bytes: response, count: strlen(response)))
        }
    }

    private func unconfirmed(_ operationID: String, _ code: String) -> Snapshot {
        Snapshot(operationID: operationID, bridgeID: bridgeID, terminal: false,
                 result: Result(ok: false, errorCode: code))
    }

    func admit(operationID: String, bridgeID: String, fingerprint: Fingerprint,
               rejection: Result? = nil) -> Admission {
        let command = WireCommand(command: "admit", operationID: operationID, bridgeID: bridgeID,
            fingerprint: WireFingerprint(action: fingerprint.action, reader: fingerprint.reader,
                                         hex: fingerprint.hex, connectionID: fingerprint.connectionID),
            rejection: rejection.map(WireResult.init))
        guard let response = call(command) else { return .reply(unconfirmed(operationID, "ble_operation_unknown")) }
        if response.dispatch, let incarnation = response.incarnation,
           let ledgerNonce = response.ledgerNonce {
            return .dispatch(Ticket(operationID: operationID, incarnation: incarnation,
                                    ledgerNonce: ledgerNonce))
        }
        return .reply(response.snapshot?.value ?? unconfirmed(operationID, "ble_operation_unknown"))
    }

    func begin(_ ticket: Ticket) -> Bool {
        call(WireCommand(command: "begin", operationID: ticket.operationID,
                         incarnation: ticket.incarnation, ledgerNonce: ticket.ledgerNonce))?.accepted == true
    }

    @discardableResult
    func finish(_ ticket: Ticket, result: Result) -> Bool {
        call(WireCommand(command: "finish", operationID: ticket.operationID,
                         incarnation: ticket.incarnation, ledgerNonce: ticket.ledgerNonce,
                         result: WireResult(result)))?.accepted == true
    }

    func registerConnection(reader: String, connectionID: String, operationID: String) -> Bool {
        call(WireCommand(command: "register_connection", operationID: operationID,
                         reader: reader, connectionID: connectionID))?.accepted == true
    }

    func confirmDisconnected(reader: String, connectionID: String, operationID: String) {
        _ = call(WireCommand(command: "confirm_disconnected", operationID: operationID,
                             reader: reader, connectionID: connectionID))
    }

    func status(operationID: String, bridgeID: String) -> Snapshot {
        call(WireCommand(command: "status", operationID: operationID,
                         bridgeID: bridgeID))?.snapshot?.value ?? unconfirmed(operationID, "ble_operation_unknown")
    }
}
