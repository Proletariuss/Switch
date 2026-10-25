import Foundation

// Deliberately not CoreBluetooth: these adapters never acquire a radio. Tests
// deliver delegate events explicitly, including omitted and delayed callbacks.
protocol ObservableObject {}
protocol CBCentralManagerDelegate: AnyObject {}
protocol CBPeripheralDelegate: AnyObject {}
let CBCentralManagerScanOptionAllowDuplicatesKey = "duplicates"
let CBAdvertisementDataLocalNameKey = "name"
let CBAdvertisementDataServiceUUIDsKey = "uuids"
enum CBManagerAuthorization { case notDetermined, restricted, denied, allowedAlways }
enum CBManager {
    static var authorization: CBManagerAuthorization = .allowedAlways
}
enum CBManagerState { case poweredOn, poweredOff, unsupported, unauthorized, unknown, resetting }
enum CBPeripheralState { case connected, disconnected, disconnecting, connecting }
enum CBCharacteristicWriteType: Equatable { case withResponse, withoutResponse }

struct CBUUID: Hashable {
    let uuidString: String
    init(string: String) { uuidString = string.uppercased() }
}

struct CBCharacteristicProperties: OptionSet {
    let rawValue: Int
    static let read = Self(rawValue: 1)
    static let write = Self(rawValue: 2)
    static let writeWithoutResponse = Self(rawValue: 4)
    static let notify = Self(rawValue: 8)
    static let indicate = Self(rawValue: 16)
}

final class CBCharacteristic {
    let uuid: CBUUID
    let properties: CBCharacteristicProperties
    var value: Data?
    var isNotifying = false
    init(_ uuid: String, _ properties: CBCharacteristicProperties) {
        self.uuid = CBUUID(string: uuid)
        self.properties = properties
    }
}

final class CBService {
    let uuid: CBUUID
    var characteristics: [CBCharacteristic]?
    init(_ uuid: String, _ characteristics: [CBCharacteristic]) {
        self.uuid = CBUUID(string: uuid)
        self.characteristics = characteristics
    }
}

final class CBPeripheral {
    let identifier: UUID
    var name: String?
    var state: CBPeripheralState = .disconnected
    var services: [CBService]?
    weak var delegate: CBPeripheralDelegate?
    var canSendWriteWithoutResponse = true
    var writes: [Data] = []
    var writeTypes: [CBCharacteristicWriteType] = []
    var notifyRequests: [CBCharacteristic] = []
    init(name: String, identifier: UUID = UUID()) {
        self.identifier = identifier
        self.name = name
    }
    func maximumWriteValueLength(for type: CBCharacteristicWriteType) -> Int { 244 }
    func writeValue(_ value: Data, for characteristic: CBCharacteristic, type: CBCharacteristicWriteType) {
        writes.append(value)
        writeTypes.append(type)
    }
    func discoverServices(_ services: [CBUUID]?) {}
    func discoverCharacteristics(_ characteristics: [CBUUID]?, for service: CBService) {}
    func setNotifyValue(_ enabled: Bool, for characteristic: CBCharacteristic) {
        notifyRequests.append(characteristic)
    }
}

final class CBCentralManager {
    var state: CBManagerState = .poweredOn
    var scanCount = 0
    var cancellations: [UUID] = []
    var cancellationObjects: [CBPeripheral] = []
    var connections: [UUID] = []
    var connectionObjects: [CBPeripheral] = []
    var connectedPeripherals: [CBPeripheral] = []
    init(delegate: CBCentralManagerDelegate?, queue: BLETestQueue) {}
    func retrieveConnectedPeripherals(withServices services: [CBUUID]) -> [CBPeripheral] {
        connectedPeripherals
    }
    func retrievePeripherals(withIdentifiers ids: [UUID]) -> [CBPeripheral] { [] }
    func scanForPeripherals(withServices services: [CBUUID]?, options: [String: Any]) {
        scanCount += 1
    }
    func stopScan() {}
    func connect(_ peripheral: CBPeripheral) {
        connections.append(peripheral.identifier)
        connectionObjects.append(peripheral)
        peripheral.state = .connecting
    }
    func cancelPeripheralConnection(_ peripheral: CBPeripheral) {
        cancellations.append(peripheral.identifier)
        cancellationObjects.append(peripheral)
        peripheral.state = .disconnecting
    }
}

final class BLETestDefaults {
    static let shared = BLETestDefaults()
    private var values: [String: String] = [:]
    func set(_ value: String, forKey key: String) { values[key] = value }
    func removeObject(forKey key: String) { values.removeValue(forKey: key) }
    func string(forKey key: String) -> String? { values[key] }
    func reset() { values.removeAll() }
}

enum ReaderOperationLogger {
    static func write(module: String, action: String, state: String, result: String, errorCode: String? = nil) {}
}

enum BLETestDiagnostics {
    static var lines: [String] = []
    static func write(_ line: String) { lines.append(line) }
    static func reset() { lines.removeAll(keepingCapacity: true) }
}

final class BLETestWork {
    private(set) var cancelled = false
    let body: () -> Void
    init(block: @escaping () -> Void) { body = block }
    func cancel() { cancelled = true }
}

// Single-owner virtual time obeys the production queue-serialization contract.
// No sleeps, run loops, radio callbacks, processes or real sockets are involved.
final class BLETestQueue {
    static var main = BLETestQueue()
    private(set) var now: Double = 0
    private var entries: [(time: Double, order: Int, work: BLETestWork)] = []
    private var order = 0
    func asyncAfter(deadline: Double, execute: BLETestWork) {
        order += 1
        entries.append((deadline, order, execute))
    }
    func advance(_ seconds: Double) {
        let target = now + seconds
        while let next = entries.enumerated().filter({ $0.element.time <= target }).min(by: {
            ($0.element.time, $0.element.order) < ($1.element.time, $1.element.order)
        }) {
            let entry = entries.remove(at: next.offset)
            now = entry.time
            if !entry.work.cancelled { entry.work.body() }
        }
        now = target
    }
}
