import Foundation

// Only the narrow system adapter is replaced. The production Unix server and
// completion ledger compile unchanged; no CoreBluetooth radio is acquired.
enum LocalBackendAuthentication {
    static let appToken = String(repeating: "test-only-", count: 4)
}

enum BluetoothReaderErrorCode: String {
    case disconnected = "ble_disconnected"
}

struct BluetoothReaderTransportError: Error {
    let code: BluetoothReaderErrorCode
}

final class BluetoothReaderManager {
    enum State { case connected, connecting, disconnected }
    static let shared = BluetoothReaderManager()
    static let readerID = UUID(uuidString: "73B7446E-E6CE-C90C-C844-485389FE8C1D")!
    var devices: [UUID: String] = [readerID: "Fixture reader"]
    var connectedID: UUID? = readerID
    var state = State.connected
    var errorCode: BluetoothReaderErrorCode?
    struct PhysicalConnectionIdentity { let reader: String; let connectionID: String; let closeOperationID: String }
    var physicalConnectionClosed: ((PhysicalConnectionIdentity) -> Void)?
    var identity = PhysicalConnectionIdentity(reader: "BLE:" + readerID.uuidString, connectionID: "connection-fixture", closeOperationID: "close-fixture")
    func connectionIdentityForBridge() -> PhysicalConnectionIdentity? { state == .connected ? identity : nil }
    private let countLock = NSLock()
    private var calls = 0
    private var holdNextCompletion = false
    private var heldCompletion: (() -> Void)?
    let cardCompletionHeld = DispatchSemaphore(value: 0)

    func holdNextCardCompletion() {
        countLock.lock(); defer { countLock.unlock() }
        precondition(!holdNextCompletion && heldCompletion == nil)
        holdNextCompletion = true
    }

    func releaseHeldCardCompletion() -> DispatchSemaphore {
        countLock.lock()
        let completion = heldCompletion
        heldCompletion = nil
        countLock.unlock()
        precondition(completion != nil)
        let finished = DispatchSemaphore(value: 0)
        DispatchQueue.main.async {
            completion!()
            // The production completion closure synchronously finishes the ledger.
            finished.signal()
        }
        return finished
    }

    var cardCalls: Int {
        countLock.lock()
        defer { countLock.unlock() }
        return calls
    }

    func matchesLifecycleTarget(_ id: UUID) -> Bool {
        connectedID == id
    }

    func scan() {}
    func connect(_ reader: UUID) { connectedID = reader; state = .connecting }
    func disconnect() { connectedID = nil; state = .disconnected }
    func isCardChannelReady(for reader: String?) -> Bool {
        state == .connected && (reader == nil || reader == "BLE:" + Self.readerID.uuidString)
    }
    func exchangeAPDU(_ data: Data, completion: @escaping (Result<Data, Error>) -> Void) {
        countLock.lock()
        calls += 1
        if holdNextCompletion {
            holdNextCompletion = false
            heldCompletion = { completion(.success(Data([0x90, 0x00]))) }
            countLock.unlock()
            cardCompletionHeld.signal()
            return
        }
        countLock.unlock()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.04) {
            completion(.success(Data([0x90, 0x00])))
        }
    }
    func powerCycle(completion: @escaping (Result<Data, Error>) -> Void) {
        exchangeAPDU(Data(), completion: completion)
    }
    func performCardOperationWithCompletionProof(
        _ operation: (@escaping (Result<Data, Error>) -> Void) -> Void,
        completion: @escaping (Result<Data, Error>) -> Void
    ) {
        operation(completion)
    }
}
